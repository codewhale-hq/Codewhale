use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use codewhale_engine::extension_host::TestManagerGuard;
use codewhale_engine::extension_host::test_support::{FixturePlugins, node_for_tests};
use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::discover_with_config;
use codewhale_plugins::host_services::{
    HostService, PersonVia, PluginApprover, ReviewApproval, TestOffer,
};
use codewhale_plugins::{HostEnvironment, PluginDiscoveryContext};
use codewhale_runtime::config::Config;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;
use tower::ServiceExt as _;
use uuid::Uuid;

use super::{
    RuntimeApiState, TestServerOverrides, build_router, ci_scaled, collect_sse_frames, mobile,
    spawn_test_server_with_root_token_mobile_workspace_and_overrides, web,
};

const REMOTE_PAIR_INDEX_MJS: &str = r#"export const name = 'remote-pair'
export const inject = ['tools', 'remote']

const outcome = async (call) => {
  try {
    return { ok: await call() }
  } catch (error) {
    return { failed: { name: error.name, code: error.code ?? null, message: error.message } }
  }
}

export function apply(ctx) {
  ctx.tools.register({
    name: 'remote_call',
    description: 'Call one method of the remote service and report the outcome.',
    parameters: {
      type: 'object',
      properties: { method: { type: 'string' }, label: { type: 'string' }, grant: { type: 'string' } },
      required: ['method'],
      additionalProperties: false,
    },
    execute: ({ method, label, grant }) => outcome(() => {
      if (method === 'status') return ctx.remote.status()
      if (method === 'pair') return ctx.remote.pair(label === undefined ? undefined : { label })
      if (method === 'clients') return ctx.remote.clients()
      if (method === 'revoke') return ctx.remote.revoke(grant)
      throw new Error(`the fixture has no remote method ${method}`)
    }),
  })
}
"#;

#[derive(Clone)]
struct PairedDeviceSession {
    cookie_pair: String,
    request_proof: String,
    stream_ticket: String,
    set_cookie: String,
}

impl PairedDeviceSession {
    fn authed_get(
        &self,
        client: &reqwest::Client,
        origin: &str,
        url: &str,
    ) -> reqwest::RequestBuilder {
        client
            .get(url)
            .header(header::COOKIE, &self.cookie_pair)
            .header(mobile::MOBILE_REQUEST_HEADER, &self.request_proof)
            .header(header::ORIGIN, origin)
            .header("sec-fetch-site", "same-origin")
    }

    fn authed_post(
        &self,
        client: &reqwest::Client,
        origin: &str,
        url: &str,
    ) -> reqwest::RequestBuilder {
        client
            .post(url)
            .header(header::COOKIE, &self.cookie_pair)
            .header(mobile::MOBILE_REQUEST_HEADER, &self.request_proof)
            .header(header::ORIGIN, origin)
            .header("sec-fetch-site", "same-origin")
    }

    fn authed_patch(
        &self,
        client: &reqwest::Client,
        origin: &str,
        url: &str,
    ) -> reqwest::RequestBuilder {
        client
            .patch(url)
            .header(header::COOKIE, &self.cookie_pair)
            .header(mobile::MOBILE_REQUEST_HEADER, &self.request_proof)
            .header(header::ORIGIN, origin)
            .header("sec-fetch-site", "same-origin")
    }

    fn authed_delete(
        &self,
        client: &reqwest::Client,
        origin: &str,
        url: &str,
    ) -> reqwest::RequestBuilder {
        client
            .delete(url)
            .header(header::COOKIE, &self.cookie_pair)
            .header(mobile::MOBILE_REQUEST_HEADER, &self.request_proof)
            .header(header::ORIGIN, origin)
            .header("sec-fetch-site", "same-origin")
    }
}

struct NodeRemoteHarness {
    _fixture: FixturePlugins,
    _policy: TestPolicyGuard,
    _offer: TestOffer,
    _manager_guard: TestManagerGuard,
    origin: String,
    token: String,
    mock: Arc<MockLlmClient>,
    client: reqwest::Client,
    state: RuntimeApiState,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for NodeRemoteHarness {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn install_reviewed_remote_pair_plugin(fixture: &FixturePlugins) -> Result<()> {
    let plugin_dir = fixture.config.user_plugins_dir.join("remote-pair");
    std::fs::create_dir_all(plugin_dir.join("native"))?;
    std::fs::write(plugin_dir.join("native/index.mjs"), REMOTE_PAIR_INDEX_MJS)?;
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"remote-pair\"\nversion = \"1.0.0\"\n[native]\npath = \"native/index.mjs\"\n[capabilities]\nhost_services = [\"remote\"]\n",
    )?;
    let mut registry = discover_with_config(&fixture.config);
    let shown = registry
        .host_service_review("remote-pair")
        .map(|review| review.token);
    registry
        .trust_reviewed(
            "remote-pair",
            ReviewApproval {
                approved_by: PluginApprover::person(PersonVia::Terminal),
                host_services: shown,
            },
        )
        .map_err(|err| anyhow::anyhow!("trust remote-pair failed: {err}"))?;
    registry
        .enable("remote-pair")
        .map_err(|err| anyhow::anyhow!("enable remote-pair failed: {err}"))?;
    Ok(())
}

impl NodeRemoteHarness {
    async fn start(test_name: &str, mobile_enabled: bool) -> Result<Option<Self>> {
        Self::start_with_mobile(test_name, mobile_enabled, None).await
    }

    async fn start_with_mobile(
        test_name: &str,
        mobile_enabled: bool,
        mobile: Option<mobile::RuntimeMobileState>,
    ) -> Result<Option<Self>> {
        let Some(node) = node_for_tests(test_name) else {
            return Ok(None);
        };
        let policy = TestPolicyGuard::extension_host(true);
        let offer = TestOffer::only(&[HostService::Remote]);
        let fixture = FixturePlugins::new(&[]).await;
        install_reviewed_remote_pair_plugin(&fixture)?;
        let mut supervision = codewhale_engine::extension_host::SupervisionOptions::default();
        supervision.ping_timeout = ci_scaled(supervision.ping_timeout);
        supervision.hang_timeout = ci_scaled(supervision.hang_timeout);
        let manager = fixture.manager_with_supervision(node, supervision);
        let manager_guard = TestManagerGuard::install(Arc::clone(&manager));

        let mut config = Config::default().with_legacy_root(
            Some("runtime-api-test-key".to_string()),
            Some("http://127.0.0.1:1/v1".to_string()),
        );
        config.yolo = Some(true);
        config.set_feature("extension_host", true)?;
        let token = format!("cwrt_remote_test_{}", Uuid::new_v4().simple());
        let (state_tx, state_rx) = oneshot::channel();

        let runtime_root = fixture.root.join("runtime-api");
        let sessions_dir = fixture.root.join("sessions");
        let Some((addr, runtime_threads, handle)) =
            spawn_test_server_with_root_token_mobile_workspace_and_overrides(
                runtime_root,
                sessions_dir,
                Some(token.clone()),
                mobile_enabled,
                fixture.workspace().to_path_buf(),
                TestServerOverrides {
                    config: Some(config),
                    mobile,
                    plugin_discovery: Some(PluginDiscoveryContext::from_config_and_environment(
                        &fixture.config,
                        HostEnvironment::from_entries(Vec::new()),
                    )),
                    state_handle: Some(state_tx),
                    ..TestServerOverrides::default()
                },
            )
            .await?
        else {
            return Ok(None);
        };

        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        runtime_threads.set_test_model_client(mock.clone());
        let state = state_rx.await.context("capture RuntimeApiState")?;
        let client = codewhale_release::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Some(Self {
            _fixture: fixture,
            _policy: policy,
            _offer: offer,
            _manager_guard: manager_guard,
            origin: format!("http://{addr}"),
            token,
            mock,
            client,
            state,
            handle,
        }))
    }

    async fn create_thread(&self) -> Result<Value> {
        let resp = self
            .client
            .post(format!("{}/v1/threads", self.origin))
            .bearer_auth(&self.token)
            .json(&json!({
                "model": "deepseek-v4-pro",
                "mode": "agent",
                "workspace": self._fixture.workspace(),
                "trust_mode": true,
                "auto_approve": true,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn list_pairings_raw(&self) -> Result<(StatusCode, String)> {
        let resp = self
            .client
            .get(format!("{}/v1/dsh/remote/pairings", self.origin))
            .bearer_auth(&self.token)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        Ok((status, body))
    }

    async fn list_pairings(&self) -> Result<Vec<Value>> {
        let (status, body) = self.list_pairings_raw().await?;
        anyhow::ensure!(
            status == StatusCode::OK,
            "list pairings failed: {status} {body}"
        );
        Ok(serde_json::from_str(&body)?)
    }

    async fn wait_thread_idle(&self, thread_id: &str) -> Result<()> {
        let deadline = Instant::now() + ci_scaled(Duration::from_secs(20));
        loop {
            let detail: Value = self
                .client
                .get(format!("{}/v1/threads/{thread_id}", self.origin))
                .bearer_auth(&self.token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let turns = detail["turns"].as_array();
            let all_terminal = turns.is_some_and(|turns| {
                turns.iter().all(|turn| {
                    matches!(
                        turn["status"].as_str(),
                        Some("completed" | "failed" | "interrupted" | "canceled")
                    ) && turn["workspace"]["state"].as_str() != Some("pending")
                })
            });
            if all_terminal {
                let mut replay = self
                    .state
                    .runtime_threads
                    .replay_events(thread_id, None, None)
                    .await?;
                let mut events = Vec::new();
                while let Some(batch) = replay.batches.recv().await {
                    events.extend(batch.map_err(anyhow::Error::msg)?);
                }
                let all_events_settled = turns.is_some_and(|turns| {
                    turns.iter().all(|turn| {
                        let Some(id) = turn["id"].as_str() else {
                            return false;
                        };
                        let ran_live = events.iter().any(|ev| {
                            ev.event == "turn.started" && ev.turn_id.as_deref() == Some(id)
                        }) || turn.get("workspace").is_some_and(|w| !w.is_null());
                        if !ran_live {
                            return true;
                        }
                        let has_completed = events.iter().any(|ev| {
                            ev.event == "turn.completed" && ev.turn_id.as_deref() == Some(id)
                        });
                        let needs_artifacts = turn["workspace"]["state"].as_str()
                            == Some("settled")
                            || turn["workspace"]["reason"].as_str() == Some("delta_failed");
                        let has_artifacts = !needs_artifacts
                            || events.iter().any(|ev| {
                                ev.event == "turn.artifacts" && ev.turn_id.as_deref() == Some(id)
                            });
                        has_completed && has_artifacts
                    })
                });
                if all_events_settled {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for thread {thread_id} to go idle: {detail}");
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn pair_on_thread(&self, thread_id: &str, label: &str) -> Result<String> {
        let before: std::collections::BTreeSet<String> = self
            .list_pairings()
            .await?
            .into_iter()
            .filter_map(|row| row["grant"].as_str().map(str::to_owned))
            .collect();

        self.mock.push_turn(canned::tool_call_turn(
            &format!("call-pair-{}", Uuid::new_v4().simple()),
            "remote_call",
            &serde_json::to_string(&json!({ "method": "pair", "label": label }))?,
        ));
        self.mock.push_turn(canned::simple_text_turn("paired"));

        let turn_resp = self
            .client
            .post(format!("{}/v1/threads/{thread_id}/turns", self.origin))
            .bearer_auth(&self.token)
            .json(&json!({ "prompt": "pair device" }))
            .send()
            .await?;
        let turn_status = turn_resp.status();
        let turn_body = turn_resp.text().await?;
        anyhow::ensure!(
            turn_status == StatusCode::CREATED,
            "start turn failed: {turn_status} {turn_body}"
        );

        self.wait_thread_idle(thread_id).await?;

        let deadline = Instant::now() + ci_scaled(Duration::from_secs(10));
        loop {
            let rows = self.list_pairings().await?;
            if let Some(grant) = rows.into_iter().find_map(|row| {
                let grant = row["grant"].as_str()?.to_string();
                let matches = row["session_id"] == thread_id
                    && row["state"] == "awaiting_host"
                    && !before.contains(&grant);
                matches.then_some(grant)
            }) {
                return Ok(grant);
            }
            if Instant::now() >= deadline {
                let mut replay = self
                    .state
                    .runtime_threads
                    .replay_events(thread_id, None, None)
                    .await?;
                let mut items = Vec::new();
                while let Some(batch) = replay.batches.recv().await {
                    for ev in batch.map_err(anyhow::Error::msg)? {
                        if ev.event.starts_with("item.") && ev.event != "item.delta" {
                            let mut line = format!("{} {}", ev.event, ev.payload);
                            line.truncate(600);
                            items.push(line);
                        }
                    }
                }
                anyhow::bail!(
                    "pairing grant never appeared for thread {thread_id}; items: {}",
                    items.join(" | ")
                );
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn present(&self, grant: &str) -> Result<reqwest::Response> {
        Ok(self
            .client
            .post(format!(
                "{}/v1/dsh/remote/pairings/{grant}/present",
                self.origin
            ))
            .bearer_auth(&self.token)
            .send()
            .await?)
    }

    async fn present_url(&self, grant: &str) -> Result<String> {
        let resp = self.present(grant).await?.error_for_status()?;
        let body: Value = resp.json().await?;
        Ok(body["url"]
            .as_str()
            .context("present url missing")?
            .to_string())
    }

    async fn withdraw(&self, grant: &str) -> Result<reqwest::Response> {
        Ok(self
            .client
            .delete(format!("{}/v1/dsh/remote/pairings/{grant}", self.origin))
            .bearer_auth(&self.token)
            .send()
            .await?)
    }

    async fn redeem_url(&self, url: &str) -> Result<PairedDeviceSession> {
        let resp = self.client.get(url).send().await?;
        anyhow::ensure!(
            resp.status() == StatusCode::SEE_OTHER,
            "unexpected redeem status {}",
            resp.status()
        );
        parse_redirect_session(resp)
    }

    async fn pair_and_redeem(
        &self,
        thread_id: &str,
        label: &str,
    ) -> Result<(String, PairedDeviceSession)> {
        let grant = self.pair_on_thread(thread_id, label).await?;
        let url = self.present_url(&grant).await?;
        let session = self.redeem_url(&url).await?;
        Ok((grant, session))
    }
}

fn parse_redirect_session(resp: reqwest::Response) -> Result<PairedDeviceSession> {
    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .context("missing Set-Cookie")?
        .to_string();
    let cookie_pair = set_cookie
        .split(';')
        .next()
        .context("missing cookie pair")?
        .to_string();
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("missing Location")?
        .to_string();
    let fragment = location
        .strip_prefix("/mobile#")
        .context("redirect must use /mobile# fragment")?;
    let request_proof = fragment
        .split('&')
        .find_map(|part| part.strip_prefix("request_proof="))
        .context("missing request_proof")?
        .to_string();
    let stream_ticket = fragment
        .split('&')
        .find_map(|part| part.strip_prefix("stream_ticket="))
        .context("missing stream_ticket")?
        .to_string();
    Ok(PairedDeviceSession {
        cookie_pair,
        request_proof,
        stream_ticket,
        set_cookie,
    })
}

struct TraceCapture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl TraceCapture {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                lines: Arc::clone(&lines),
            },
            lines,
        )
    }
}

struct FieldStringCollector(Vec<String>);

impl tracing::field::Visit for FieldStringCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.0.push(format!("{}={:?}", field.name(), value));
    }
}

impl tracing::Subscriber for TraceCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut collector = FieldStringCollector(Vec::new());
        event.record(&mut collector);
        let line = format!("{} {}", event.metadata().target(), collector.0.join(" "));
        if let Ok(mut lines) = self.lines.lock() {
            lines.push(line);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[tokio::test]
async fn a_pairing_is_listed_without_a_secret_and_presented_once() -> Result<()> {
    let Some(harness) = NodeRemoteHarness::start(
        "a_pairing_is_listed_without_a_secret_and_presented_once",
        true,
    )
    .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let grant = harness.pair_on_thread(thread_id, "kitchen tablet").await?;

    let (status, raw_list) = harness.list_pairings_raw().await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!raw_list.contains("cwrp_"), "{raw_list}");
    assert!(!raw_list.contains("cwrc_"), "{raw_list}");

    let rows: Vec<Value> = serde_json::from_str(&raw_list)?;
    let row = rows
        .iter()
        .find(|item| item["grant"] == grant)
        .context("listed grant")?;
    assert_eq!(row["plugin_name"], "remote-pair");
    assert_eq!(row["session_id"], thread_id);
    assert_eq!(row["state"], "awaiting_host");
    assert_eq!(row["plugin_label"], "kitchen tablet");
    assert!(row["client_label"].is_null());
    assert!(row["plugin_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(
        row["expires_in_seconds"]
            .as_u64()
            .is_some_and(|secs| secs > 0)
    );

    let first_present = harness.present(&grant).await?;
    assert_eq!(first_present.status(), StatusCode::OK);
    assert_eq!(
        first_present
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let first_body: Value = first_present.json().await?;
    assert_eq!(first_body["grant"], grant);
    let first_url = first_body["url"].as_str().context("first url")?.to_string();
    assert!(
        first_url.starts_with(&format!("{}/__codewhale/mobile/pair/cwrp_", harness.origin)),
        "{first_url}"
    );

    let second_url = harness.present_url(&grant).await?;
    assert_ne!(first_url, second_url);

    let first_redeem = harness.client.get(&first_url).send().await?;
    assert_eq!(first_redeem.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(first_redeem.text().await?, "bootstrap unavailable");

    let device = harness.redeem_url(&second_url).await?;
    assert!(
        device
            .cookie_pair
            .starts_with("codewhale_mobile_session=cwms_")
    );
    assert!(
        device
            .set_cookie
            .contains("HttpOnly; SameSite=Strict; Path=/")
    );

    let (status_after, raw_after) = harness.list_pairings_raw().await?;
    assert_eq!(status_after, StatusCode::OK);
    assert!(!raw_after.contains("cwrp_"));
    assert!(!raw_after.contains("cwrc_"));
    let rows_after: Vec<Value> = serde_json::from_str(&raw_after)?;
    let paired_row = rows_after
        .iter()
        .find(|item| item["grant"] == grant)
        .context("paired row")?;
    assert_eq!(paired_row["state"], "paired");
    assert!(paired_row["client_label"].as_str().is_some());

    let third_present = harness.present(&grant).await?;
    assert_eq!(third_present.status(), StatusCode::NOT_FOUND);
    let third_body = third_present.text().await?;
    assert!(!third_body.contains(&grant), "{third_body}");

    Ok(())
}

#[tokio::test]
async fn the_three_pairing_routes_need_the_master_bearer() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let token = format!("cwrt_master_test_{}", Uuid::new_v4().simple());
    let (web_state, web_nonce) = web::RuntimeWebState::new();
    let (web_cookie, web_proof) = web_state
        .consume(&web_nonce, "127.0.0.1".parse().unwrap())
        .map_err(|err| anyhow::anyhow!("consume web nonce failed: {err:?}"))?;
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            sessions_dir.clone(),
            Some(token.clone()),
            true,
            workspace.clone(),
            TestServerOverrides {
                web: Some(web_state),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let origin = format!("http://{addr}");
    let client = codewhale_release::tls::reqwest_client();

    let mobile_resp = client
        .post(format!("{origin}/__codewhale/mobile/session"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?;
    let mobile_cookie_pair = mobile_resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .context("mobile cookie")?
        .to_string();
    let mobile_session: Value = mobile_resp.json().await?;
    let owner_mobile_proof = mobile_session["request_proof"]
        .as_str()
        .context("owner proof")?
        .to_string();

    let watch_resp: Value = client
        .post(format!("{origin}/v1/auth/client-tokens"))
        .bearer_auth(&token)
        .json(&json!({ "device_id": "watch-dev", "intent": "watch" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let watch_token = watch_resp["token"]
        .as_str()
        .context("watch token")?
        .to_string();

    let drive_resp: Value = client
        .post(format!("{origin}/v1/auth/client-tokens"))
        .bearer_auth(&token)
        .json(&json!({ "device_id": "drive-dev", "intent": "drive" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let drive_token = drive_resp["token"]
        .as_str()
        .context("drive token")?
        .to_string();

    let fake_grant = format!("cwrg_{}", "01".repeat(32));
    let endpoints = [
        (Method::GET, format!("{origin}/v1/dsh/remote/pairings")),
        (
            Method::POST,
            format!("{origin}/v1/dsh/remote/pairings/{fake_grant}/present"),
        ),
        (
            Method::DELETE,
            format!("{origin}/v1/dsh/remote/pairings/{fake_grant}"),
        ),
    ];

    for (method, url) in &endpoints {
        let unauthed = client.request(method.clone(), url).send().await?;
        assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED, "{url}");

        let wrong_bearer = client
            .request(method.clone(), url)
            .bearer_auth("wrong-bearer")
            .send()
            .await?;
        assert_eq!(wrong_bearer.status(), StatusCode::UNAUTHORIZED, "{url}");

        let alias_header = client
            .request(method.clone(), url)
            .header("x-codewhale-runtime-token", &token)
            .send()
            .await?;
        assert_eq!(alias_header.status(), StatusCode::FORBIDDEN, "{url}");

        let watch = client
            .request(method.clone(), url)
            .bearer_auth(&watch_token)
            .send()
            .await?;
        assert_eq!(watch.status(), StatusCode::FORBIDDEN, "{url}");

        let drive = client
            .request(method.clone(), url)
            .bearer_auth(&drive_token)
            .send()
            .await?;
        assert_eq!(drive.status(), StatusCode::FORBIDDEN, "{url}");

        let web_req = client
            .request(method.clone(), url)
            .header(
                header::COOKIE,
                format!("codewhale_web_session={web_cookie}"),
            )
            .header(web::WEB_REQUEST_HEADER, &web_proof)
            .header(header::ORIGIN, &origin)
            .header("sec-fetch-site", "same-origin")
            .send()
            .await?;
        assert_eq!(web_req.status(), StatusCode::FORBIDDEN, "{url}");

        let mobile_req = client
            .request(method.clone(), url)
            .header(header::COOKIE, &mobile_cookie_pair)
            .header(mobile::MOBILE_REQUEST_HEADER, &owner_mobile_proof)
            .header(header::ORIGIN, &origin)
            .header("sec-fetch-site", "same-origin")
            .send()
            .await?;
        assert_eq!(mobile_req.status(), StatusCode::FORBIDDEN, "{url}");
    }

    assert_eq!(
        client
            .get(&endpoints[0].1)
            .bearer_auth(&token)
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .post(&endpoints[1].1)
            .bearer_auth(&token)
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .delete(&endpoints[2].1)
            .bearer_auth(&token)
            .send()
            .await?
            .status(),
        StatusCode::NO_CONTENT
    );

    handle.abort();

    let Some((no_auth_addr, _, no_auth_handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.join("no-auth"),
            root.join("no-auth-sessions"),
            None,
            true,
            workspace,
            TestServerOverrides::default(),
        )
        .await?
    else {
        return Ok(());
    };
    let no_auth_origin = format!("http://{no_auth_addr}");
    for (method, path) in [
        (Method::GET, "/v1/dsh/remote/pairings".to_string()),
        (
            Method::POST,
            format!("/v1/dsh/remote/pairings/{fake_grant}/present"),
        ),
        (
            Method::DELETE,
            format!("/v1/dsh/remote/pairings/{fake_grant}"),
        ),
    ] {
        let resp = client
            .request(method, format!("{no_auth_origin}{path}"))
            .send()
            .await?;
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{path}");
        let body = resp.text().await?;
        assert!(!body.contains("Watch turns"), "{body}");
    }
    no_auth_handle.abort();

    Ok(())
}

#[tokio::test]
async fn a_paired_device_reaches_only_its_own_conversation() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("a_paired_device_reaches_only_its_own_conversation", true).await?
    else {
        return Ok(());
    };
    let thread_a = harness.create_thread().await?;
    let thread_b = harness.create_thread().await?;
    let id_a = thread_a["id"].as_str().context("thread a")?.to_string();
    let id_b = thread_b["id"].as_str().context("thread b")?.to_string();

    let (grant, device) = harness.pair_and_redeem(&id_a, "tablet").await?;

    let list_resp = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads", harness.origin),
        )
        .send()
        .await?
        .error_for_status()?;
    let list_rows: Vec<Value> = list_resp.json().await?;
    assert_eq!(list_rows.len(), 1);
    assert_eq!(list_rows[0]["id"], id_a);

    let summary_resp = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!(
                "{}/v1/threads/summary?limit=60&include_archived=false",
                harness.origin
            ),
        )
        .send()
        .await?
        .error_for_status()?;
    let summary_rows: Vec<Value> = summary_resp.json().await?;
    assert_eq!(summary_rows.len(), 1);
    assert_eq!(summary_rows[0]["id"], id_a);

    let foreign_filter_summary = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/summary?thread_ids={id_b}", harness.origin),
        )
        .send()
        .await?
        .error_for_status()?;
    let foreign_rows: Vec<Value> = foreign_filter_summary.json().await?;
    assert!(foreign_rows.is_empty(), "{foreign_rows:?}");

    let own_detail = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{id_a}", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(own_detail.status(), StatusCode::OK);

    let foreign_detail = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{id_b}", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(foreign_detail.status(), StatusCode::FORBIDDEN);

    harness
        .mock
        .push_turn(canned::simple_text_turn("reply from paired device"));
    let turn_resp = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{id_a}/turns", harness.origin),
        )
        .json(&json!({ "prompt": "hello from phone" }))
        .send()
        .await?;
    assert_eq!(turn_resp.status(), StatusCode::CREATED);
    let turn_json: Value = turn_resp.json().await?;
    let turn_id = turn_json["turn"]["id"]
        .as_str()
        .context("turn id")?
        .to_string();
    harness.wait_thread_idle(&id_a).await?;

    let interrupt_foreign = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!(
                "{}/v1/threads/{id_b}/turns/{turn_id}/interrupt",
                harness.origin
            ),
        )
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(interrupt_foreign.status(), StatusCode::FORBIDDEN);

    let own_interrupt = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!(
                "{}/v1/threads/{id_a}/turns/{turn_id}/interrupt",
                harness.origin
            ),
        )
        .json(&json!({}))
        .send()
        .await?;
    assert_ne!(own_interrupt.status(), StatusCode::FORBIDDEN);
    assert_ne!(own_interrupt.status(), StatusCode::UNAUTHORIZED);

    let own_steer = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{id_a}/turns/{turn_id}/steer", harness.origin),
        )
        .json(&json!({ "prompt": "follow up" }))
        .send()
        .await?;
    assert_ne!(own_steer.status(), StatusCode::FORBIDDEN);
    assert_ne!(own_steer.status(), StatusCode::UNAUTHORIZED);

    let foreign_turn = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{id_b}/turns", harness.origin),
        )
        .json(&json!({ "prompt": "nope" }))
        .send()
        .await?;
    assert_eq!(foreign_turn.status(), StatusCode::FORBIDDEN);

    let foreign_stream = harness
        .client
        .get(format!(
            "{}/v1/threads/{id_b}/events?{}={}",
            harness.origin,
            mobile::MOBILE_STREAM_TICKET_QUERY,
            device.stream_ticket
        ))
        .header(header::COOKIE, &device.cookie_pair)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(foreign_stream.status(), StatusCode::FORBIDDEN);

    let own_stream = harness
        .client
        .get(format!(
            "{}/v1/threads/{id_a}/events?{}={}",
            harness.origin,
            mobile::MOBILE_STREAM_TICKET_QUERY,
            device.stream_ticket
        ))
        .header(header::COOKIE, &device.cookie_pair)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(own_stream.status(), StatusCode::OK);
    drop(own_stream);

    let refreshed = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/__codewhale/mobile/stream-ticket", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(refreshed.status(), StatusCode::OK);

    assert_eq!(
        device
            .authed_post(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads", harness.origin),
            )
            .json(&json!({}))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        device
            .authed_patch(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{id_a}", harness.origin),
            )
            .json(&json!({ "archived": true }))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        device
            .authed_delete(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{id_a}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    for route in [
        format!("{}/v1/config", harness.origin),
        format!("{}/v1/workspace/status", harness.origin),
        format!("{}/v1/apps/plugins", harness.origin),
        format!("{}/v1/dsh/remote/pairings", harness.origin),
        format!("{}/v1/threads/{id_a}/terminals", harness.origin),
    ] {
        assert_eq!(
            device
                .authed_get(&harness.client, &harness.origin, &route)
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN,
            "{route}"
        );
    }
    assert_eq!(
        device
            .authed_post(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/dsh/remote/pairings/{grant}/present", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        device
            .authed_delete(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/dsh/remote/pairings/{grant}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    let sibling_port = harness
        .client
        .get(format!("{}/v1/threads/{id_a}", harness.origin))
        .header(header::COOKIE, &device.cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &device.request_proof)
        .header(header::ORIGIN, "http://127.0.0.1:3000")
        .header("sec-fetch-site", "same-site")
        .send()
        .await?;
    assert_eq!(sibling_port.status(), StatusCode::UNAUTHORIZED);

    let cookie_only = harness
        .client
        .get(format!("{}/v1/threads/{id_a}", harness.origin))
        .header(header::COOKIE, &device.cookie_pair)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(cookie_only.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn a_paired_device_cannot_answer_an_approval_card() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("a_paired_device_cannot_answer_an_approval_card", true).await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let (_grant, device) = harness.pair_and_redeem(thread_id, "tablet").await?;

    let post_approval = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/approvals/approval-missing", harness.origin),
        )
        .json(&json!({ "decision": "allow", "remember": false }))
        .send()
        .await?;
    assert_eq!(post_approval.status(), StatusCode::FORBIDDEN);

    let list_approvals = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/approvals", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(list_approvals.status(), StatusCode::FORBIDDEN);

    let user_input = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/user-input/{thread_id}/input-1", harness.origin),
        )
        .json(&json!({ "answers": {} }))
        .send()
        .await?;
    assert_eq!(user_input.status(), StatusCode::FORBIDDEN);

    let owner_resp = harness
        .client
        .post(format!("{}/__codewhale/mobile/session", harness.origin))
        .bearer_auth(&harness.token)
        .send()
        .await?
        .error_for_status()?;
    let owner_cookie = owner_resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .context("owner cookie")?
        .to_string();
    let owner_body: Value = owner_resp.json().await?;
    let owner_proof = owner_body["request_proof"]
        .as_str()
        .context("owner proof")?
        .to_string();

    let owner_approval = harness
        .client
        .post(format!("{}/v1/approvals/approval-missing", harness.origin))
        .header(header::COOKIE, &owner_cookie)
        .header(mobile::MOBILE_REQUEST_HEADER, &owner_proof)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .json(&json!({ "decision": "allow", "remember": false }))
        .send()
        .await?;
    assert_eq!(owner_approval.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn revoking_the_plugin_ends_the_device_session_at_once() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("revoking_the_plugin_ends_the_device_session_at_once", true)
            .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let (_grant, device) = harness.pair_and_redeem(thread_id, "tablet").await?;

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    let revoke_resp = harness
        .client
        .post(format!(
            "{}/v1/apps/plugins/remote-pair/revoke",
            harness.origin
        ))
        .bearer_auth(&harness.token)
        .send()
        .await?;
    assert_eq!(revoke_resp.status(), StatusCode::OK);

    let deadline = Instant::now() + ci_scaled(Duration::from_secs(10));
    loop {
        if harness.list_pairings().await?.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("plugin revoke did not withdraw remote grant in time");
        }
        sleep(Duration::from_millis(25)).await;
    }

    let first_after = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{thread_id}", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(first_after.status(), StatusCode::UNAUTHORIZED);

    let second_after = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/__codewhale/mobile/stream-ticket", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(second_after.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn archiving_the_conversation_ends_the_device_session() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("archiving_the_conversation_ends_the_device_session", true)
            .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let (_grant, device) = harness.pair_and_redeem(thread_id, "tablet").await?;

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    let archive_resp = harness
        .client
        .patch(format!("{}/v1/threads/{thread_id}", harness.origin))
        .bearer_auth(&harness.token)
        .json(&json!({ "archived": true }))
        .send()
        .await?;
    assert_eq!(archive_resp.status(), StatusCode::OK);
    assert!(harness.list_pairings().await?.is_empty());

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let unarchive_resp = harness
        .client
        .patch(format!("{}/v1/threads/{thread_id}", harness.origin))
        .bearer_auth(&harness.token)
        .json(&json!({ "archived": false }))
        .send()
        .await?;
    assert_eq!(unarchive_resp.status(), StatusCode::OK);

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    Ok(())
}

#[tokio::test]
async fn the_host_withdrawal_route_ends_a_paired_device() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("the_host_withdrawal_route_ends_a_paired_device", true).await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let (grant, device) = harness.pair_and_redeem(thread_id, "tablet").await?;

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    let delete_known = harness.withdraw(&grant).await?;
    let known_status = delete_known.status();
    let known_body = delete_known.text().await?;
    assert_eq!(known_status, StatusCode::NO_CONTENT);

    let delete_again = harness.withdraw(&grant).await?;
    assert_eq!(delete_again.status(), known_status);
    assert_eq!(delete_again.text().await?, known_body);

    let delete_never = harness
        .withdraw(&format!("cwrg_{}", "22".repeat(32)))
        .await?;
    assert_eq!(delete_never.status(), known_status);
    assert_eq!(delete_never.text().await?, known_body);

    assert!(harness.list_pairings().await?.is_empty());

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    Ok(())
}

#[tokio::test]
async fn a_replayed_expired_or_malformed_link_is_refused_alike() -> Result<()> {
    let Some(harness) = NodeRemoteHarness::start(
        "a_replayed_expired_or_malformed_link_is_refused_alike",
        true,
    )
    .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;

    let grant_replay = harness.pair_on_thread(thread_id, "replay phone").await?;
    let replay_url = harness.present_url(&grant_replay).await?;
    let _device = harness.redeem_url(&replay_url).await?;

    let grant_withdrawn = harness.pair_on_thread(thread_id, "withdrawn phone").await?;
    let withdrawn_url = harness.present_url(&grant_withdrawn).await?;
    assert_eq!(
        harness.withdraw(&grant_withdrawn).await?.status(),
        StatusCode::NO_CONTENT
    );

    let bad_urls = [
        format!("{}/__codewhale/mobile/pair/not-a-secret", harness.origin),
        format!(
            "{}/__codewhale/mobile/pair/cwrp_{}",
            harness.origin,
            "00".repeat(32)
        ),
        replay_url,
        withdrawn_url,
    ];
    for url in bad_urls {
        let resp = harness.client.get(&url).send().await?;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{url}");
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "{url}"
        );
        assert_eq!(resp.text().await?, "bootstrap unavailable", "{url}");
    }

    let (short_mobile, _nonce) = mobile::RuntimeMobileState::new_with_ttls(
        Duration::from_secs(60),
        Duration::ZERO,
        Duration::from_secs(60),
    );
    let mut expired_state = harness.state.clone();
    expired_state.mobile = Some(short_mobile.clone());
    let expired_bootstrap = short_mobile.issue_session();
    assert_eq!(expired_bootstrap.session_ttl_seconds, 0);
    let expired_req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/threads/{thread_id}"))
        .header(
            header::COOKIE,
            format!(
                "{}={}",
                mobile::MOBILE_SESSION_COOKIE_NAME,
                expired_bootstrap.session_cookie
            ),
        )
        .header(
            mobile::MOBILE_REQUEST_HEADER,
            &expired_bootstrap.request_proof,
        )
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .body(Body::empty())?;
    let expired_resp = build_router(expired_state).oneshot(expired_req).await?;
    assert_eq!(expired_resp.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn a_link_opened_from_a_non_loopback_peer_is_refused() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("a_link_opened_from_a_non_loopback_peer_is_refused", true).await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let grant = harness.pair_on_thread(thread_id, "remote tablet").await?;
    let url = harness.present_url(&grant).await?;
    let path = url
        .strip_prefix(&harness.origin)
        .context("present path")?
        .to_string();

    let non_loopback: SocketAddr = "198.51.100.9:4444".parse().unwrap();
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(&path)
        .header("x-forwarded-for", "127.0.0.1")
        .body(Body::empty())?;
    req.extensions_mut().insert(ConnectInfo(non_loopback));

    let resp = build_router(harness.state.clone()).oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = axum::body::to_bytes(resp.into_body(), 4096).await?;
    assert_eq!(body.as_ref(), b"bootstrap unavailable");

    let loopback = harness.redeem_url(&url).await?;
    assert_eq!(
        loopback
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    Ok(())
}

#[tokio::test]
async fn a_stream_ticket_of_a_withdrawn_grant_opens_no_stream() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("a_stream_ticket_of_a_withdrawn_grant_opens_no_stream", true)
            .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let (grant, device) = harness.pair_and_redeem(thread_id, "stream tablet").await?;

    assert_eq!(
        harness.withdraw(&grant).await?.status(),
        StatusCode::NO_CONTENT
    );

    let stream_resp = harness
        .client
        .get(format!(
            "{}/v1/threads/{thread_id}/events?{}={}",
            harness.origin,
            mobile::MOBILE_STREAM_TICKET_QUERY,
            device.stream_ticket
        ))
        .header(header::COOKIE, &device.cookie_pair)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?;
    assert_eq!(stream_resp.status(), StatusCode::UNAUTHORIZED);

    let followup = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{thread_id}", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(followup.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn the_device_token_appears_in_no_cookie_url_body_or_log() -> Result<()> {
    let Some(harness) = NodeRemoteHarness::start(
        "the_device_token_appears_in_no_cookie_url_body_or_log",
        true,
    )
    .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let grant = harness.pair_on_thread(thread_id, "audit phone").await?;

    let (subscriber, captured_logs) = TraceCapture::new();
    let _log_guard = tracing::subscriber::set_default(subscriber);

    let router = build_router(harness.state.clone());

    let list_req = Request::builder()
        .method(Method::GET)
        .uri("/v1/dsh/remote/pairings")
        .header(header::AUTHORIZATION, format!("Bearer {}", harness.token))
        .body(Body::empty())?;
    let list_resp = router.clone().oneshot(list_req).await?;
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_text = String::from_utf8(
        axum::body::to_bytes(list_resp.into_body(), 1024 * 64)
            .await?
            .to_vec(),
    )?;
    assert!(!list_text.contains("cwrp_"));
    assert!(!list_text.contains("cwrc_"));

    let present_req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/dsh/remote/pairings/{grant}/present"))
        .header(header::AUTHORIZATION, format!("Bearer {}", harness.token))
        .body(Body::empty())?;
    let present_resp = router.clone().oneshot(present_req).await?;
    assert_eq!(present_resp.status(), StatusCode::OK);
    let present_bytes = axum::body::to_bytes(present_resp.into_body(), 1024 * 64).await?;
    let present_json: Value = serde_json::from_slice(&present_bytes)?;
    let url = present_json["url"].as_str().context("present url")?;
    let secret = url
        .split("/__codewhale/mobile/pair/")
        .nth(1)
        .context("secret segment")?
        .to_string();
    assert!(secret.starts_with("cwrp_"));
    let present_text = String::from_utf8(present_bytes.to_vec())?;
    assert!(!present_text.contains("cwrc_"));

    let loopback: SocketAddr = "127.0.0.1:50001".parse().unwrap();
    let mut redeem_req = Request::builder()
        .method(Method::GET)
        .uri(format!("/__codewhale/mobile/pair/{secret}"))
        .body(Body::empty())?;
    redeem_req.extensions_mut().insert(ConnectInfo(loopback));
    let redeem_resp = router.clone().oneshot(redeem_req).await?;
    assert_eq!(redeem_resp.status(), StatusCode::SEE_OTHER);
    let set_cookie = redeem_resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .context("Set-Cookie")?
        .to_string();
    let location = redeem_resp
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("Location")?
        .to_string();
    let redeem_body = String::from_utf8(
        axum::body::to_bytes(redeem_resp.into_body(), 4096)
            .await?
            .to_vec(),
    )?;
    for surface in [&set_cookie, &location, &redeem_body] {
        assert!(!surface.contains("cwrc_"), "{surface}");
        assert!(!surface.contains(&secret), "{surface}");
    }

    let cookie_pair = set_cookie
        .split(';')
        .next()
        .context("cookie pair")?
        .to_string();
    let fragment = location.strip_prefix("/mobile#").context("fragment")?;
    let proof = fragment
        .split('&')
        .find_map(|part| part.strip_prefix("request_proof="))
        .context("proof")?
        .to_string();

    let thread_req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/threads/{thread_id}"))
        .header(header::COOKIE, &cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &proof)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .body(Body::empty())?;
    let thread_resp = router.clone().oneshot(thread_req).await?;
    assert_eq!(thread_resp.status(), StatusCode::OK);
    let thread_text = String::from_utf8(
        axum::body::to_bytes(thread_resp.into_body(), 1024 * 64)
            .await?
            .to_vec(),
    )?;
    assert!(!thread_text.contains("cwrc_"));
    assert!(!thread_text.contains(&secret));

    let forbidden_req = Request::builder()
        .method(Method::POST)
        .uri("/v1/approvals/any")
        .header(header::COOKIE, &cookie_pair)
        .header(mobile::MOBILE_REQUEST_HEADER, &proof)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))?;
    let forbidden_resp = router.clone().oneshot(forbidden_req).await?;
    assert_eq!(forbidden_resp.status(), StatusCode::FORBIDDEN);
    let forbidden_text = String::from_utf8(
        axum::body::to_bytes(forbidden_resp.into_body(), 4096)
            .await?
            .to_vec(),
    )?;
    assert!(!forbidden_text.contains("cwrc_"));
    assert!(!forbidden_text.contains(&secret));

    let logs = captured_logs.lock().unwrap().join("\n");
    assert!(!logs.contains("cwrc_"), "{logs}");
    assert!(!logs.contains(&secret), "{logs}");
    assert!(!logs.contains("cwms_"), "{logs}");
    assert!(!logs.contains("cwmr_"), "{logs}");
    assert!(!logs.contains("cwmt_"), "{logs}");

    Ok(())
}

#[tokio::test]
async fn the_grant_session_is_the_runtime_thread_id() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("the_grant_session_is_the_runtime_thread_id", true).await?
    else {
        return Ok(());
    };
    let saved_id = "sess_remote_thread_id_proof";
    let session_doc = json!({
        "schema_version": 1,
        "metadata": {
            "id": saved_id,
            "title": "Remote Thread Identity Proof",
            "created_at": "2026-11-14T00:00:00Z",
            "updated_at": "2026-11-14T00:00:00Z",
            "message_count": 2,
            "total_tokens": 100,
            "workspace": harness._fixture.workspace(),
            "model": "deepseek-v4-pro",
            "mode": "agent"
        },
        "messages": [
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Hello" }]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Ready" }]
            }
        ],
        "system_prompt": null
    });
    std::fs::write(
        harness.state.sessions_dir.join(format!("{saved_id}.json")),
        serde_json::to_string_pretty(&session_doc)?,
    )?;

    let resumed: Value = harness
        .client
        .post(format!(
            "{}/v1/sessions/{saved_id}/resume-thread",
            harness.origin
        ))
        .bearer_auth(&harness.token)
        .json(&json!({ "model": "deepseek-v4-pro" }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let runtime_thread_id = resumed["thread_id"]
        .as_str()
        .context("resumed thread id")?
        .to_string();
    assert_ne!(runtime_thread_id, saved_id);

    harness
        .client
        .patch(format!("{}/v1/threads/{runtime_thread_id}", harness.origin))
        .bearer_auth(&harness.token)
        .json(&json!({ "trust_mode": true, "auto_approve": true }))
        .send()
        .await?
        .error_for_status()?;

    let (grant, device) = harness
        .pair_and_redeem(&runtime_thread_id, "desk phone")
        .await?;
    let rows = harness.list_pairings().await?;
    let row = rows
        .iter()
        .find(|item| item["grant"] == grant)
        .context("pairing row")?;
    assert_eq!(row["session_id"], runtime_thread_id);
    assert_ne!(row["session_id"], saved_id);

    assert_eq!(
        device
            .authed_get(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{runtime_thread_id}", harness.origin),
            )
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    Ok(())
}

#[tokio::test]
async fn present_refuses_when_mobile_control_is_off() -> Result<()> {
    let Some(harness) =
        NodeRemoteHarness::start("present_refuses_when_mobile_control_is_off", false).await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let grant = harness.pair_on_thread(thread_id, "waiting phone").await?;

    let present_mobile_off = harness.present(&grant).await?;
    assert_eq!(present_mobile_off.status(), StatusCode::CONFLICT);
    let rows = harness.list_pairings().await?;
    let row = rows
        .iter()
        .find(|item| item["grant"] == grant)
        .context("still awaiting host")?;
    assert_eq!(row["state"], "awaiting_host");

    let foreign_root = tempfile::tempdir()?;
    let mut foreign_config = Config::default().with_legacy_root(
        Some("runtime-api-test-key".to_string()),
        Some("http://127.0.0.1:1/v1".to_string()),
    );
    foreign_config.yolo = Some(true);
    let (other_state_tx, other_state_rx) = oneshot::channel();
    let Some((_other_addr, _other_threads, other_handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            foreign_root.path().join("other-runtime"),
            foreign_root.path().join("other-sessions"),
            Some(harness.token.clone()),
            true,
            foreign_root.path().to_path_buf(),
            TestServerOverrides {
                config: Some(foreign_config),
                state_handle: Some(other_state_tx),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let other_state = other_state_rx.await.context("capture other state")?;
    let other_router = build_router(other_state);
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/dsh/remote/pairings/{grant}/present"))
        .header(header::AUTHORIZATION, format!("Bearer {}", harness.token))
        .body(Body::empty())?;
    let resp = other_router.oneshot(req).await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    other_handle.abort();

    Ok(())
}

async fn open_live_paired_sse_stream(
    harness: &NodeRemoteHarness,
    device: &PairedDeviceSession,
    thread_id: &str,
) -> Result<(
    mpsc::UnboundedReceiver<(String, Value)>,
    tokio::task::JoinHandle<Result<Vec<(String, Value)>>>,
)> {
    let response = harness
        .client
        .get(format!(
            "{}/v1/threads/{thread_id}/events?since_seq=0&progress=true&{}={}",
            harness.origin,
            mobile::MOBILE_STREAM_TICKET_QUERY,
            device.stream_ticket
        ))
        .header(header::COOKIE, &device.cookie_pair)
        .header(header::ORIGIN, &harness.origin)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await?
        .error_for_status()?;
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
    let collector = tokio::spawn(collect_sse_frames(response, frame_tx));
    tokio::time::timeout(ci_scaled(Duration::from_secs(5)), async {
        while let Some((name, payload)) = frame_rx.recv().await {
            if name == "stream.progress" && payload["state"] == "live" {
                return Ok(());
            }
        }
        anyhow::bail!("paired SSE stream closed before becoming live")
    })
    .await
    .context("paired SSE stream never went live")??;
    Ok((frame_rx, collector))
}

async fn assert_stream_ended_without_events(
    collector: tokio::task::JoinHandle<Result<Vec<(String, Value)>>>,
    thread_id: &str,
    expected_last_seq: u64,
    forbidden_events: &[&str],
) -> Result<()> {
    let frames = tokio::time::timeout(ci_scaled(Duration::from_secs(8)), collector)
        .await
        .context("paired SSE stream stayed open after access ended")???;
    for event_name in forbidden_events {
        assert!(
            frames.iter().all(|(name, _)| name != event_name),
            "forbidden post-revocation event {event_name} leaked into SSE frames: {frames:?}"
        );
    }
    let ends: Vec<_> = frames
        .iter()
        .filter(|(name, _)| name == "stream.end")
        .collect();
    assert_eq!(ends.len(), 1, "expected exactly one stream.end: {frames:?}");
    let (end_name, end_payload) = frames.last().context("expected trailing stream.end")?;
    assert_eq!(end_name, "stream.end");
    assert_eq!(end_payload["thread_id"], thread_id);
    assert_eq!(end_payload["reason"], "remote_access_revoked");
    assert_eq!(end_payload["retryable"], false);
    assert_eq!(end_payload["last_seq"], expected_last_seq);
    Ok(())
}

#[tokio::test]
async fn a_paired_device_turn_cannot_override_posture_or_escalate_ask() -> Result<()> {
    let Some(harness) = NodeRemoteHarness::start(
        "a_paired_device_turn_cannot_override_posture_or_escalate_ask",
        true,
    )
    .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?;
    let (_grant, device) = harness.pair_and_redeem(thread_id, "phone").await?;
    let updated: Value = harness
        .client
        .patch(format!("{}/v1/threads/{thread_id}", harness.origin))
        .bearer_auth(&harness.token)
        .json(&json!({
            "permission_posture": "ask",
            "allow_shell": false,
            "trust_mode": false,
            "auto_approve": false,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(updated["permission_posture"], "ask");
    assert_eq!(updated["auto_approve"], false);

    let forbidden_overrides = [
        json!({ "prompt": "hi", "mode": "plan" }),
        json!({ "prompt": "hi", "permission_posture": "bypass" }),
        json!({ "prompt": "hi", "allowed_tools": ["exec_command"] }),
        json!({ "prompt": "hi", "allowed_tools": [] }),
        json!({
            "prompt": "hi",
            "dynamic_tools": [{
                "name": "override_tool",
                "description": "forbidden remote tool",
                "input_schema": { "type": "object" },
            }],
        }),
        json!({ "prompt": "hi", "model": "other-model" }),
        json!({ "prompt": "hi", "model_provider": "openai" }),
        json!({ "prompt": "hi", "model_provider_id": "custom-1" }),
        json!({ "prompt": "hi", "account_model_owner": "acct-1" }),
        json!({
            "prompt": "hi",
            "profile_constitution": {
                "accountId": "acct-1",
                "revision": 1,
                "constitution": {
                    "schemaVersion": 1,
                    "detail": "balanced",
                    "initiative": "judgment",
                    "collaboration": "direct",
                    "notes": "override",
                },
            },
        }),
        json!({ "prompt": "hi", "reasoning_effort": "high" }),
        json!({ "prompt": "hi", "max_output_tokens": 2048 }),
        json!({ "prompt": "hi", "maxOutputTokens": 2048 }),
        json!({ "prompt": "hi", "environment_id": "env-1" }),
        json!({ "prompt": "hi", "auto_approve": true }),
        json!({ "prompt": "hi", "allow_shell": true }),
        json!({ "prompt": "hi", "trust_mode": true }),
    ];
    for body in forbidden_overrides {
        let resp = device
            .authed_post(
                &harness.client,
                &harness.origin,
                &format!("{}/v1/threads/{thread_id}/turns", harness.origin),
            )
            .json(&body)
            .send()
            .await?;
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "expected 403 for override payload {body}"
        );
    }

    harness
        .mock
        .push_turn(canned::simple_text_turn("kept ask posture"));
    let allowed_resp = device
        .authed_post(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{thread_id}/turns", harness.origin),
        )
        .json(&json!({
            "prompt": "hello from mobile html",
            "allow_shell": false,
            "trust_mode": false,
            "auto_approve": false,
        }))
        .send()
        .await?;
    assert_eq!(allowed_resp.status(), StatusCode::CREATED);
    let turn_json: Value = allowed_resp.json().await?;
    assert_eq!(turn_json["turn"]["permission_posture"], "ask");
    assert_eq!(turn_json["thread"]["permission_posture"], "ask");
    assert_eq!(turn_json["thread"]["auto_approve"], false);
    harness.wait_thread_idle(thread_id).await?;

    let detail: Value = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{thread_id}", harness.origin),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["thread"]["permission_posture"], "ask");
    assert_eq!(detail["thread"]["auto_approve"], false);

    Ok(())
}

#[tokio::test]
async fn an_open_sse_stream_ends_cleanly_on_withdrawal_archive_and_plugin_revoke() -> Result<()> {
    let Some(harness) = NodeRemoteHarness::start(
        "an_open_sse_stream_ends_cleanly_on_withdrawal_archive_and_plugin_revoke",
        true,
    )
    .await?
    else {
        return Ok(());
    };

    let t1 = harness.create_thread().await?;
    let id1 = t1["id"].as_str().context("t1 id")?.to_string();
    let (grant1, device1) = harness.pair_and_redeem(&id1, "withdraw phone").await?;
    let e1 = harness
        .state
        .runtime_threads
        .emit_event_for_test(&id1, None, "item.completed", json!({ "n": 1 }))
        .await?;
    let (_rx1, collector1) = open_live_paired_sse_stream(&harness, &device1, &id1).await?;
    assert_eq!(
        harness.withdraw(&grant1).await?.status(),
        StatusCode::NO_CONTENT
    );
    harness
        .state
        .runtime_threads
        .emit_event_for_test(
            &id1,
            None,
            "thread.updated",
            json!({ "changes": { "title": "post-withdraw" } }),
        )
        .await?;
    assert_stream_ended_without_events(collector1, &id1, e1.seq, &["thread.updated"]).await?;

    let t2 = harness.create_thread().await?;
    let id2 = t2["id"].as_str().context("t2 id")?.to_string();
    let (_grant2, device2) = harness.pair_and_redeem(&id2, "archive tablet").await?;
    let e2 = harness
        .state
        .runtime_threads
        .emit_event_for_test(&id2, None, "item.completed", json!({ "n": 2 }))
        .await?;
    let (_rx2, collector2) = open_live_paired_sse_stream(&harness, &device2, &id2).await?;
    let archive_resp = harness
        .client
        .patch(format!("{}/v1/threads/{id2}", harness.origin))
        .bearer_auth(&harness.token)
        .json(&json!({ "archived": true }))
        .send()
        .await?;
    assert_eq!(archive_resp.status(), StatusCode::OK);
    harness
        .state
        .runtime_threads
        .emit_event_for_test(
            &id2,
            None,
            "approval.grant_revoked",
            json!({ "grant_id": "g-archive" }),
        )
        .await?;
    assert_stream_ended_without_events(
        collector2,
        &id2,
        e2.seq,
        &["thread.updated", "approval.grant_revoked"],
    )
    .await?;

    let t3 = harness.create_thread().await?;
    let id3 = t3["id"].as_str().context("t3 id")?.to_string();
    let (_grant3, device3) = harness.pair_and_redeem(&id3, "revoke watch").await?;
    let e3 = harness
        .state
        .runtime_threads
        .emit_event_for_test(&id3, None, "item.completed", json!({ "n": 3 }))
        .await?;
    let (_rx3, collector3) = open_live_paired_sse_stream(&harness, &device3, &id3).await?;
    let revoke_resp = harness
        .client
        .post(format!(
            "{}/v1/apps/plugins/remote-pair/revoke",
            harness.origin
        ))
        .bearer_auth(&harness.token)
        .send()
        .await?;
    assert_eq!(revoke_resp.status(), StatusCode::OK);
    assert_stream_ended_without_events(
        collector3,
        &id3,
        e3.seq,
        &["thread.updated", "approval.grant_revoked"],
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn an_open_sse_stream_ends_cleanly_on_session_ttl_expiry() -> Result<()> {
    let (short_mobile, _nonce) = mobile::RuntimeMobileState::new_with_ttls(
        Duration::from_secs(60),
        ci_scaled(Duration::from_secs(3)),
        Duration::from_secs(60),
    );
    let Some(harness) = NodeRemoteHarness::start_with_mobile(
        "an_open_sse_stream_ends_cleanly_on_session_ttl_expiry",
        true,
        Some(short_mobile),
    )
    .await?
    else {
        return Ok(());
    };
    let thread = harness.create_thread().await?;
    let thread_id = thread["id"].as_str().context("thread id")?.to_string();
    let (_grant, device) = harness.pair_and_redeem(&thread_id, "ttl phone").await?;
    let event = harness
        .state
        .runtime_threads
        .emit_event_for_test(&thread_id, None, "item.completed", json!({ "n": 4 }))
        .await?;
    let (_rx, collector) = open_live_paired_sse_stream(&harness, &device, &thread_id).await?;
    assert_stream_ended_without_events(collector, &thread_id, event.seq, &[]).await?;

    let after = device
        .authed_get(
            &harness.client,
            &harness.origin,
            &format!("{}/v1/threads/{thread_id}", harness.origin),
        )
        .send()
        .await?;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn insecure_no_auth_rejects_invalid_or_wrong_origin_credentials() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().to_path_buf();
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let (web_state, web_nonce) = web::RuntimeWebState::new();
    let (web_cookie, web_proof) = web_state
        .consume(&web_nonce, "127.0.0.1".parse().unwrap())
        .map_err(|err| anyhow::anyhow!("consume web nonce failed: {err:?}"))?;
    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root,
            sessions_dir,
            None,
            true,
            workspace,
            TestServerOverrides {
                web: Some(web_state),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let origin = format!("http://{addr}");
    let client = codewhale_release::tls::reqwest_client();
    let threads_url = format!("{origin}/v1/threads");

    assert_eq!(
        client.get(&threads_url).send().await?.status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(&threads_url)
            .header(
                header::COOKIE,
                format!("codewhale_web_session={web_cookie}")
            )
            .header(web::WEB_REQUEST_HEADER, &web_proof)
            .header(header::ORIGIN, &origin)
            .header("sec-fetch-site", "same-origin")
            .send()
            .await?
            .status(),
        StatusCode::OK
    );

    assert_eq!(
        client
            .get(&threads_url)
            .bearer_auth("cwct_revoked_or_unknown")
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&threads_url)
            .header("x-codewhale-runtime-token", "bogus")
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&threads_url)
            .header("x-deepseek-runtime-token", "bogus")
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&threads_url)
            .header(
                header::COOKIE,
                format!("codewhale_web_session={web_cookie}")
            )
            .header(web::WEB_REQUEST_HEADER, &web_proof)
            .header(header::ORIGIN, "http://127.0.0.1:3000")
            .header("sec-fetch-site", "same-origin")
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!(
                "{origin}/v1/threads/thread-1/events?web_stream_ticket=cwwt_bogus"
            ))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&threads_url)
            .header(header::COOKIE, "codewhale_mobile_session=cwms_revoked")
            .header(mobile::MOBILE_REQUEST_HEADER, "cwmr_revoked")
            .header(header::ORIGIN, "http://127.0.0.1:3000")
            .header("sec-fetch-site", "same-origin")
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!(
                "{origin}/v1/threads/thread-1/events?mobile_stream_ticket=cwmt_revoked"
            ))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let mut multi_mobile_cookies = reqwest::header::HeaderMap::new();
    multi_mobile_cookies.append(header::COOKIE, "innocent=1".parse()?);
    multi_mobile_cookies.append(
        header::COOKIE,
        "codewhale_mobile_session=cwms_revoked".parse()?,
    );
    assert_eq!(
        client
            .get(&threads_url)
            .headers(multi_mobile_cookies)
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let mut multi_web_cookies = reqwest::header::HeaderMap::new();
    multi_web_cookies.append(header::COOKIE, "innocent=1".parse()?);
    multi_web_cookies.append(
        header::COOKIE,
        "codewhale_web_session=cwws_revoked".parse()?,
    );
    assert_eq!(
        client
            .get(&threads_url)
            .headers(multi_web_cookies)
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    handle.abort();
    Ok(())
}
