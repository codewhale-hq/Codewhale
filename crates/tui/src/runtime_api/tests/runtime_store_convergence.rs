//! Real Engine/store through the actual authenticated owner dispatcher. Only
//! the model is scripted; this is not a fake HTTP bridge or an independent lease.
use super::*;
use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
use codewhale_engine::session_manager::SessionManager;

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn reply(
    guest: &mut codewhale_app_server::daemon_client::OwnerClient,
    id: &str,
) -> Result<Value> {
    tokio::time::timeout(ci_scaled(Duration::from_secs(60)), async {
        loop {
            let frame = guest.recv().await?.context("owner closed before reply")?;
            if frame["id"] == id {
                return Ok(frame);
            }
        }
    })
    .await
    .context("owner reply timed out")?
}

#[tokio::test]
async fn authenticated_owner_guest_runs_real_engine_and_reuses_held_store() -> Result<()> {
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let config_path = root.join("config.toml");
    fs::write(&config_path, "model = 'deepseek-v4-pro'\n")?;
    let config = Config::default().with_legacy_root(
        Some("owner-fixture-key".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    #[cfg(unix)]
    let socket_path = root.join("private-run").join("owner.sock");
    #[cfg(windows)]
    let socket_path = PathBuf::from(format!(r"\\.\pipe\codewhale-test-{}", Uuid::new_v4()));
    let shutdown = RuntimeServerShutdown::default();
    let token = format!("owner-fixture-{}", Uuid::new_v4());
    let (addr, threads, http) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root.clone(),
        root.join("sessions"),
        Some(token.clone()),
        false,
        root.join("workspace"),
        TestServerOverrides {
            config: Some(config),
            config_path: Some(config_path.clone()),
            owner_socket: Some(socket_path.clone()),
            shutdown: Some(shutdown.clone()),
            ..TestServerOverrides::default()
        },
    )
    .await?
    .context("owner acceptance requires a real loopback listener")?;
    let _http_abort = AbortOnDrop(http.abort_handle());
    let model = Arc::new(MockLlmClient::new(Vec::new()));
    threads.set_test_model_client(model.clone());
    let manager = threads.clone();
    let (binding, generation) =
        codewhale_app_server::daemon_socket::owner_work(move || manager.capture_control_owner())
            .await?;
    let owner_client = codewhale_app_server::daemon_client::connect(
        Some(config_path.clone()),
        Some(socket_path.clone()),
    )
    .await?;
    let captured = owner_client.receipt().clone();
    assert_eq!(captured.data_dir, binding.data_dir);
    assert_eq!(captured.execution_scope, binding.execution_scope);
    assert_eq!(captured.lease_generation, generation);
    assert_eq!(captured.socket_path, socket_path);
    assert_eq!(captured.config_path, Some(config_path.clone()));
    assert_eq!(captured.pid, std::process::id());
    assert_eq!(
        captured.process_start,
        codewhale_app_server::daemon_socket::capture_process_start(std::process::id()).await?
    );
    #[cfg(unix)]
    assert_eq!(
        captured.principal,
        codewhale_config::private_directory::PrivateDirectory::current_user_id().to_string()
    );
    #[cfg(windows)]
    assert_eq!(
        captured.principal,
        codewhale_app_server::daemon_socket::owner_work(|| {
            codewhale_config::windows_identity::CurrentWindowsUser::open()?.sid_string()
        })
        .await?
    );
    drop(owner_client);
    let acp = codewhale_app_server::daemon_client::connect_acp_if_published(
        Some(config_path.clone()),
        Some(socket_path.clone()),
    )
    .await?
    .context("actual Normal owner admits its captured ACP frontend")?;
    assert_eq!(acp.receipt(), &captured);
    tokio::time::timeout(
        Duration::from_secs(5),
        acp.forward(tokio::io::empty(), tokio::io::sink()),
    )
    .await??;
    let manager = threads.clone();
    let (after_acp_eof, after_generation) =
        codewhale_app_server::daemon_socket::owner_work(move || manager.capture_control_owner())
            .await?;
    assert_eq!(after_acp_eof.data_dir, captured.data_dir);
    assert_eq!(after_generation, captured.lease_generation);
    assert!(!threads.is_acp_host());
    let unauthenticated = codewhale_release::tls::reqwest_client()
        .get(format!("http://{addr}/v1/threads"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    // Compatibility prompts address an admitted durable thread, never an
    // invented alias that could remint an existing conversation.
    let mut creator = codewhale_app_server::daemon_client::connect(
        Some(config_path.clone()),
        Some(socket_path.clone()),
    )
    .await?;
    assert_eq!(creator.receipt(), &captured);
    creator
        .send(
            json!("create"),
            "thread/create",
            json!({"metadata":{"operation_key":"same-owner-conversation-create","model":"deepseek-v4-pro"}}),
        )
        .await?;
    let created = reply(&mut creator, "create").await?;
    assert!(created["error"].is_null(), "{created:#}");
    let conversation = created["result"]["thread_id"]
        .as_str()
        .context("durable creation returns its canonical identity")?
        .to_owned();
    drop(creator);
    for (id, answer) in [
        ("first", "first owner answer"),
        ("second", "second owner answer"),
    ] {
        model.push_turn(canned::simple_text_turn(answer));
        let mut guest = codewhale_app_server::daemon_client::connect(
            Some(config_path.clone()),
            Some(socket_path.clone()),
        )
        .await?;
        assert_eq!(guest.receipt(), &captured);
        guest
            .send(
                json!(id),
                "prompt/request",
                json!({"thread_id":conversation,"allowed_tools":[],"prompt":id,"model":"deepseek-v4-pro"}),
            )
            .await?;
        let result = reply(&mut guest, id).await?;
        assert!(result["error"].is_null(), "{result:#}");
        assert_eq!(result["result"]["output"], answer);
        guest
            .send(json!("guest-shutdown"), "shutdown", json!({}))
            .await?;
        assert!(reply(&mut guest, "guest-shutdown").await?["error"].is_object());
        // Actual per-connection logical EOF on Windows and Unix; host stays up.
        tokio::time::timeout(
            Duration::from_secs(5),
            guest.forward(tokio::io::empty(), tokio::io::sink()),
        )
        .await??;
    }
    let catalog: Value = codewhale_release::tls::reqwest_client()
        .get(format!("http://{addr}/v1/threads"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = catalog.as_array().context("canonical thread catalog")?;
    assert_eq!(rows.len(), 1, "reattachment must not remint Runtime/store");
    assert_eq!(rows[0]["id"], conversation);
    let detail = threads
        .get_thread_detail(rows[0]["id"].as_str().context("thread id")?)
        .await?;
    assert_eq!(detail.turns.len(), 2);
    assert!(
        detail
            .turns
            .iter()
            .all(|turn| serde_json::to_value(turn).is_ok_and(|v| v["status"] == "completed"))
    );
    assert!(detail.latest_seq > 0);
    let manager = threads.clone();
    let (after, generation) =
        codewhale_app_server::daemon_socket::owner_work(move || manager.capture_control_owner())
            .await?;
    assert_eq!(after.data_dir, captured.data_dir);
    assert_eq!(generation, captured.lease_generation);
    // Join the actual owned Engines while their hosting runtime is still alive.
    threads.shutdown_and_wait().await?;
    assert!(shutdown.drain(Duration::from_secs(5)).await);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if codewhale_app_server::daemon_client::connect(
                Some(config_path.clone()),
                Some(socket_path.clone()),
            )
            .await
            .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("withdrawal must refuse without child/fresh-store fallback")?;
    threads.shutdown_and_wait().await?;
    http.abort();
    Ok(())
}

struct GatedOwnerModel {
    scripted: MockLlmClient,
    first: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl codewhale_engine::llm_client::LlmClient for GatedOwnerModel {
    fn provider_name(&self) -> &'static str {
        "deepseek"
    }
    fn model(&self) -> &str {
        "deepseek-v4-pro"
    }
    async fn create_message(
        &self,
        request: codewhale_models::MessageRequest,
    ) -> Result<codewhale_models::MessageResponse> {
        codewhale_engine::llm_client::LlmClient::create_message(&self.scripted, request).await
    }
    async fn create_message_stream(
        &self,
        request: codewhale_models::MessageRequest,
    ) -> Result<codewhale_engine::llm_client::StreamEventBox> {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.acquire().await?.forget();
        }
        codewhale_engine::llm_client::LlmClient::create_message_stream(&self.scripted, request)
            .await
    }
}
fn listener_selection(
    workspace: PathBuf,
    port: u16,
    token: String,
) -> codewhale_app_server::RuntimeListenerSelection {
    codewhale_app_server::RuntimeListenerSelection {
        workers: 1,
        workspace,
        config_profile: None,
        config_source: None,
        host: "127.0.0.1".into(),
        port,
        cors_origins: Vec::new(),
        auth_token: Some(token),
        insecure_no_auth: false,
        mobile: false,
        web: false,
    }
}
fn authenticated_http(token: &str) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    Ok(codewhale_release::platform_http_client_builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
async fn frontend_ready(
    guest: &mut codewhale_app_server::daemon_client::OwnerClient,
) -> Result<RuntimeFrontendReady> {
    let frame = tokio::time::timeout(Duration::from_secs(10), guest.recv())
        .await??
        .context("owner closed before frontend readiness")?;
    anyhow::ensure!(
        frame["method"] == "daemon/frontend_ready" && frame.get("id").is_none(),
        "invalid frontend readiness"
    );
    Ok(serde_json::from_value(frame["params"].clone())?)
}

#[tokio::test]
async fn real_normal_owner_distinct_listener_detach_keeps_turn_compat_acp_and_successor()
-> Result<()> {
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let workspace = root.join("workspace");
    #[cfg(unix)]
    let socket = root.join("private-run").join("normal-owner.sock");
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\normal-owner-{}", Uuid::new_v4()));
    let original_token = "cwrt_original_owner_only";
    let selected_token = "cwrt_selected_listener_only";
    let config = Config::default().with_legacy_root(
        Some("fixture-key".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    let (original, manager, server) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            Some(original_token.into()),
            false,
            workspace.clone(),
            TestServerOverrides {
                config: Some(config),
                owner_socket: Some(socket.clone()),
                ..Default::default()
            },
        )
        .await?
        .context("real listener required")?;
    let _abort = AbortOnDrop(server.abort_handle());
    let model = Arc::new(GatedOwnerModel {
        scripted: MockLlmClient::new(vec![
            canned::simple_text_turn("pending turn survived detach"),
            canned::simple_text_turn("compat answer"),
            canned::simple_text_turn("ACP answer"),
            canned::simple_text_turn("ordinary successor"),
        ]),
        first: std::sync::atomic::AtomicBool::new(true),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    manager.set_test_model_client(model.clone());
    let control = codewhale_app_server::daemon_client::connect(None, Some(socket.clone())).await?;
    let receipt = control.receipt().clone();
    validate_selected_owner(&control, receipt.data_dir.clone()).await?;
    drop(control);
    let pending_workspace = root.join("pending-workspace");
    fs::create_dir(&pending_workspace)?;
    let mut guest = codewhale_app_server::daemon_client::connect_listener_if_published(
        None,
        Some(socket.clone()),
        listener_selection(pending_workspace.clone(), 0, selected_token.into()),
        receipt.clone(),
    )
    .await?
    .context("published owner")?;
    let ready = frontend_ready(&mut guest).await?;
    assert_ne!(ready.endpoint, original);
    assert!(!ready.reused_owner_listener);
    let selected = authenticated_http(selected_token)?;
    let original_http = authenticated_http(original_token)?;
    assert_eq!(
        selected
            .get(format!("http://{original}/v1/threads"))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        original_http
            .get(format!("http://{}/v1/threads", ready.endpoint))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let thread: Value = selected
        .post(format!("http://{}/v1/threads", ready.endpoint))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(thread["workspace"].as_str(), pending_workspace.to_str());
    let id = thread["id"].as_str().context("thread id")?.to_owned();
    let started: Value = selected
        .post(format!("http://{}/v1/threads/{id}/turns", ready.endpoint))
        .json(&json!({"prompt":"persist across listener close"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn = started["turn"]["id"]
        .as_str()
        .context("turn id")?
        .to_owned();
    tokio::time::timeout(Duration::from_secs(30), model.entered.notified()).await?;
    drop(guest);
    tokio::time::timeout(Duration::from_secs(10), async {
        while tokio::net::TcpStream::connect(ready.endpoint).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert_eq!(
        original_http
            .get(format!("http://{original}/healthz"))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    assert!(!manager.is_acp_host());
    model.release.add_permits(1);
    assert_eq!(
        wait_for_terminal_turn_status(
            &original_http,
            original,
            &id,
            &turn,
            Duration::from_secs(30)
        )
        .await?,
        "completed"
    );
    // Compatibility uses the same captured original bridge and actual Engine.
    let selected_workspace = root.join("compat-workspace");
    fs::create_dir(&selected_workspace)?;
    let mut compat = codewhale_app_server::daemon_client::connect_scoped_control_if_published(
        None,
        Some(socket.clone()),
        codewhale_app_server::RuntimeFrontendScope {
            workers: 1,
            workspace: selected_workspace.clone(),
            config_profile: None,
            config_source: None,
        },
        receipt.clone(),
    )
    .await?
    .context("captured scoped control")?;
    compat
        .send(
            json!("compat-create"),
            "thread/create",
            json!({"metadata":{"operation_key":"scoped-compat-create"}}),
        )
        .await?;
    let created = reply(&mut compat, "compat-create").await?;
    assert!(created["error"].is_null(), "{created:#}");
    let compat_id = created["result"]["thread_id"]
        .as_str()
        .context("scoped creation returns its canonical identity")?
        .to_owned();
    compat
        .send(
            json!("compat"),
            "prompt/request",
            json!({"thread_id":compat_id,"allowed_tools":[],"prompt":"same owner compatibility"}),
        )
        .await?;
    let result = reply(&mut compat, "compat").await?;
    assert!(result["error"].is_null(), "{result:#}");
    assert_eq!(result["result"]["output"], "compat answer");
    drop(compat);
    let mut acp = codewhale_app_server::daemon_client::connect_selected_acp_if_published(
        None,
        Some(socket.clone()),
        codewhale_app_server::RuntimeFrontendScope {
            workers: 1,
            workspace: workspace.clone(),
            config_profile: None,
            config_source: None,
        },
        "deepseek-v4-pro".into(),
        receipt.clone(),
    )
    .await?
    .context("same Normal owner ACP")?;
    acp.send(
        json!("init"),
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{}}),
    )
    .await?;
    assert!(reply(&mut acp, "init").await?["error"].is_null());
    acp.send(
        json!("new"),
        "session/new",
        json!({"cwd":workspace,"mcpServers":[]}),
    )
    .await?;
    let session = reply(&mut acp, "new").await?["result"]["sessionId"]
        .as_str()
        .context("ACP session")?
        .to_owned();
    acp.send(
        json!("prompt"),
        "session/prompt",
        json!({"sessionId":session,"prompt":"one narrowed turn"}),
    )
    .await?;
    assert_eq!(
        reply(&mut acp, "prompt").await?["result"]["stopReason"],
        "end_turn"
    );
    drop(acp);
    let successor: Value = original_http
        .post(format!("http://{original}/v1/threads/{id}/turns"))
        .json(&json!({"prompt":"ordinary successor"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let successor_id = successor["turn"]["id"].as_str().context("successor id")?;
    assert_eq!(
        wait_for_terminal_turn_status(
            &original_http,
            original,
            &id,
            successor_id,
            Duration::from_secs(30)
        )
        .await?,
        "completed"
    );
    assert_eq!(
        model.scripted.call_count(),
        4,
        "no replay after guest detach"
    );
    assert_eq!(
        model.scripted.captured_requests()[2].model,
        "deepseek-v4-pro"
    );
    let catalog: Value = original_http
        .get(format!("http://{original}/v1/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        catalog
            .as_array()
            .context("canonical catalog")?
            .iter()
            .any(|row| row["workspace"].as_str() == selected_workspace.to_str()),
        "scoped control default must reach the same canonical thread store"
    );

    let captured = manager.clone();
    let (binding, generation) =
        codewhale_app_server::daemon_socket::owner_work(move || captured.capture_control_owner())
            .await?;
    assert_eq!(generation, receipt.lease_generation);
    assert_eq!(binding.data_dir, receipt.data_dir);
    let detail = manager.get_thread_detail(&id).await?;
    assert_eq!(detail.turns.len(), 2);
    assert!(detail.latest_seq > 0);
    manager.shutdown_and_wait().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn real_owner_exact_listener_reuse_scope_refusal_and_fresh_browser_projection() -> Result<()>
{
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let workspace = root.join("workspace");
    #[cfg(unix)]
    let socket = root.join("private-run").join("browser-owner.sock");
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\browser-owner-{}", Uuid::new_v4()));
    let token = "cwrt_bound_operator_secret";
    let (original, manager, server) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            Some(token.into()),
            false,
            workspace.clone(),
            TestServerOverrides {
                owner_socket: Some(socket.clone()),
                ..Default::default()
            },
        )
        .await?
        .context("real listener required")?;
    let _abort = AbortOnDrop(server.abort_handle());
    let control = codewhale_app_server::daemon_client::connect(None, Some(socket.clone())).await?;
    let receipt = control.receipt().clone();
    drop(control);
    let same = listener_selection(workspace.clone(), original.port(), token.into());
    assert!(!format!("{same:?}").contains(token));
    let mut reuse = codewhale_app_server::daemon_client::connect_listener_if_published(
        None,
        Some(socket.clone()),
        same.clone(),
        receipt.clone(),
    )
    .await?
    .context("published owner")?;
    let ready = frontend_ready(&mut reuse).await?;
    assert_eq!(ready.endpoint, original);
    assert!(ready.reused_owner_listener);
    drop(reuse);
    let client = authenticated_http(token)?;
    assert_eq!(
        client
            .get(format!("http://{original}/v1/threads"))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    let mut wrong = same;
    wrong.workers = 2;
    assert!(
        codewhale_app_server::daemon_client::connect_listener_if_published(
            None,
            Some(socket.clone()),
            wrong,
            receipt.clone()
        )
        .await
        .is_err()
    );
    assert!(
        codewhale_app_server::daemon_client::connect_scoped_control_if_published(
            None,
            Some(socket.clone()),
            codewhale_app_server::RuntimeFrontendScope {
                workers: 1,
                workspace: workspace.clone(),
                config_profile: Some("unadmitted-profile".into()),
                config_source: None,
            },
            receipt.clone()
        )
        .await
        .is_err()
    );
    let mut stale = receipt.clone();
    stale.lease_generation.push_str("-stale");
    assert!(
        codewhale_app_server::daemon_client::connect_listener_if_published(
            None,
            Some(socket.clone()),
            listener_selection(workspace.clone(), 0, token.into()),
            stale
        )
        .await
        .is_err()
    );
    // A distinct selected root now joins its actual owner-held scope.
    let other_workspace = root.join("other-workspace");
    fs::create_dir(&other_workspace)?;
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    drop(reservation);
    let mut browser = listener_selection(other_workspace.clone(), port, token.into());
    browser.web = true;
    browser.mobile = true;
    let mut guest = codewhale_app_server::daemon_client::connect_listener_if_published(
        None,
        Some(socket.clone()),
        browser,
        receipt.clone(),
    )
    .await?
    .context("published owner")?;
    let selected = frontend_ready(&mut guest).await?;
    assert!(!selected.reused_owner_listener);
    let web_url = selected.web_bootstrap_url.context("fresh web bootstrap")?;
    let mobile_url = selected
        .mobile_bootstrap_url
        .context("fresh mobile bootstrap")?;
    assert!(!web_url.contains(token) && !mobile_url.contains(token));
    let boot = client.get(&web_url).send().await?;
    assert_eq!(boot.status(), StatusCode::SEE_OTHER);
    let cookie = boot.headers()[header::SET_COOKIE]
        .to_str()?
        .split(';')
        .next()
        .context("web cookie")?
        .to_owned();
    let proof = boot.headers()[header::LOCATION]
        .to_str()?
        .strip_prefix("/#p=")
        .context("web proof")?
        .to_owned();
    assert_eq!(
        client.get(&web_url).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    // Cookies/proofs are admitted only at this selected origin and cannot
    // replace the original listener's bearer gate.
    let unauth = codewhale_release::tls::reqwest_client();
    let selected_origin = format!("http://{}", selected.endpoint);
    let created: Value = unauth
        .post(format!("{selected_origin}/v1/threads"))
        .header(header::COOKIE, &cookie)
        .header(web::WEB_REQUEST_HEADER, &proof)
        .header(header::ORIGIN, &selected_origin)
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(created["workspace"].as_str(), other_workspace.to_str());
    assert_eq!(
        unauth
            .get(format!("http://{original}/v1/threads"))
            .header(header::COOKIE, &cookie)
            .header(web::WEB_REQUEST_HEADER, &proof)
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mobile = client.get(&mobile_url).send().await?;
    assert_eq!(mobile.status(), StatusCode::SEE_OTHER);
    assert!(
        !mobile.headers()[header::SET_COOKIE]
            .to_str()?
            .contains(token)
    );
    assert_eq!(
        client.get(&mobile_url).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    drop(guest);
    assert_eq!(
        client
            .get(format!("http://{original}/v1/threads"))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    manager.shutdown_and_wait().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn real_owner_two_workspace_services_share_scope_caches_and_settle_global_mutations()
-> Result<()> {
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let original_workspace = root.join("original-workspace");
    let selected_workspace = root.join("selected-workspace");
    fs::create_dir_all(&original_workspace)?;
    fs::create_dir_all(&selected_workspace)?;
    fs::write(original_workspace.join("marker.txt"), "original contents")?;
    fs::write(selected_workspace.join("marker.txt"), "selected contents")?;
    #[cfg(unix)]
    let socket = root.join("private-run").join("scope-owner.sock");
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\scope-owner-{}", Uuid::new_v4()));
    let token = "cwrt_scope_operator_only";
    let workers = runtime_api_sub_agent_manager(&original_workspace, 2)?;
    let (scope_tx, scope_rx) = oneshot::channel();
    let (original, manager, server) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            Some(token.into()),
            false,
            original_workspace.clone(),
            TestServerOverrides {
                owner_socket: Some(socket.clone()),
                sub_agent_manager: Some(workers.clone()),
                workspace_scopes_handle: Some(scope_tx),
                ..Default::default()
            },
        )
        .await?
        .context("real original listener required")?;
    let _abort = AbortOnDrop(server.abort_handle());
    let scopes = scope_rx.await?;
    assert!(
        Arc::ptr_eq(&scopes.workers, &workers),
        "workspace service admission must reuse the real owner's worker actor"
    );

    let control = codewhale_app_server::daemon_client::connect(None, Some(socket.clone())).await?;
    let receipt = control.receipt().clone();
    drop(control);
    let mut first = codewhale_app_server::daemon_client::connect_listener_if_published(
        None,
        Some(socket.clone()),
        listener_selection(selected_workspace.clone(), 0, token.into()),
        receipt.clone(),
    )
    .await?
    .context("first selected listener")?;
    let first_ready = frontend_ready(&mut first).await?;
    let mut second = codewhale_app_server::daemon_client::connect_listener_if_published(
        None,
        Some(socket.clone()),
        listener_selection(selected_workspace.clone(), 0, token.into()),
        receipt,
    )
    .await?
    .context("matching selected listener")?;
    let second_ready = frontend_ready(&mut second).await?;
    assert_ne!(first_ready.endpoint, second_ready.endpoint);
    let client = authenticated_http(token)?;
    for (address, workspace, contents) in [
        (original, &original_workspace, "original contents"),
        (
            first_ready.endpoint,
            &selected_workspace,
            "selected contents",
        ),
        (
            second_ready.endpoint,
            &selected_workspace,
            "selected contents",
        ),
    ] {
        let file: Value = client
            .get(format!(
                "http://{address}/v1/workspace/files/read?path=marker.txt"
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(file["content"], contents);
        let lsp: Value = client
            .get(format!("http://{address}/v1/lsp"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(lsp["workspace"].as_str(), workspace.to_str());
    }
    let (original_scope, selected_scope) = {
        let held = scopes.scopes.lock();
        assert_eq!(
            held.len(),
            2,
            "two matching listeners must not create two scopes"
        );
        (
            held[&original_workspace].clone(),
            held[&selected_workspace].clone(),
        )
    };
    for endpoint in [original, second_ready.endpoint] {
        let runs: Value = client
            .get(format!("http://{endpoint}/v1/agent-runs"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(
            runs["governor"]["max_launch_slots"], 2,
            "both selected roots must report the same owner-global worker ceiling"
        );
    }
    // Run a real Engine turn in the selected workspace, then retain a live
    // direct child under that exact owner/session while its execution cwd is
    // an isolated directory. Endpoint scope must come from admission receipt.
    let model = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "origin-owner-turn",
    )]));
    manager.set_test_model_client(model.clone());
    let selected_thread: Value = client
        .post(format!("http://{}/v1/threads", second_ready.endpoint))
        .json(&json!({"model":"deepseek-v4-pro"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let selected_session = selected_thread["id"]
        .as_str()
        .context("selected owner session")?
        .to_owned();
    let turn: Value = client
        .post(format!(
            "http://{}/v1/threads/{selected_session}/turns",
            second_ready.endpoint
        ))
        .json(&json!({"prompt":"prove selected owner Engine"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        wait_for_terminal_turn_status(
            &client,
            second_ready.endpoint,
            &selected_session,
            turn["turn"]["id"].as_str().context("selected turn")?,
            Duration::from_secs(30)
        )
        .await?,
        "completed"
    );
    assert_eq!(model.call_count(), 1);
    let selected_fleet = FleetManager::open(&selected_workspace)?
        .with_sub_agent_manager(workers.clone())
        .with_session_model(DEFAULT_TEXT_MODEL)
        .with_route_config(test_fleet_route_config());
    let task: codewhale_protocol::fleet::FleetTaskSpec = serde_json::from_value(json!({
        "id":"scope-lease", "name":"scope lease", "description":null,
        "objective":"hold a reviewed local Fleet admission", "instructions":"no process launched",
        "worker":{"agent_profile":null,"role":"reviewer","loadout":null,"model_class":null,
            "model":null,"tool_profile":"read-only","tools":[],"capabilities":[]},
        "workspace":null,"input_files":[],"context":[],"budget":null,"tags":[],
        "expected_artifacts":[],"scorer":null,"retry_policy":null,"alert_policy":null,
        "timeout_seconds":null,"metadata":{}
    }))?;
    let report = selected_fleet.create_run(
        codewhale_engine::fleet::task_spec::FleetTaskSpecDocument {
            name: Some("selected lease".into()),
            labels: Default::default(),
            security_policy: None,
            workers: Vec::new(),
            tasks: vec![task],
            usage_ceiling: None,
        },
        1,
    )?;
    let ledger = selected_fleet.rebuild_state()?;
    let fleet_record = workers
        .read()
        .await
        .fleet_worker_records_for_workspace(&selected_workspace)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .find(|record| record.spec.run_id == report.run_id.0)
        .context("actual Fleet admission record")?;
    assert!(fleet_worker_has_selected_lease(&fleet_record, &ledger));
    let mut wrong_worker = ledger.clone();
    wrong_worker
        .tasks
        .values_mut()
        .next()
        .context("actual Fleet task")?
        .leased_to = Some("different-live-worker".into());
    assert!(!fleet_worker_has_selected_lease(
        &fleet_record,
        &wrong_worker
    ));
    let mut wrong_run = ledger;
    wrong_run
        .tasks
        .values_mut()
        .next()
        .context("actual Fleet task")?
        .entry
        .run_id
        .0 = "different-run".into();
    assert!(!fleet_worker_has_selected_lease(&fleet_record, &wrong_run));
    assert_eq!(
        client
            .get(format!(
                "http://{original}/v1/agent-runs/{}",
                fleet_record.spec.run_id
            ))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .get(format!(
                "http://{}/v1/agent-runs/{}",
                second_ready.endpoint, fleet_record.spec.run_id
            ))
            .send()
            .await?
            .status(),
        StatusCode::OK
    );
    let execution_workspace = root.join("isolated-direct-execution");
    fs::create_dir(&execution_workspace)?;
    let mut actor = workers.clone().write_owned().await;
    let selected_root = selected_workspace.clone();
    let direct_id = codewhale_app_server::daemon_socket::owner_work(move || {
        actor
            .insert_test_running_direct_child_in_origin(
                "scope-direct",
                &execution_workspace,
                &selected_root,
                &selected_session,
            )
            .map_err(anyhow::Error::msg)
    })
    .await?;
    for address in [first_ready.endpoint, second_ready.endpoint] {
        let record: Value = client
            .get(format!("http://{address}/v1/agent-runs/{direct_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_ne!(
            record["spec"]["workspace"].as_str(),
            selected_workspace.to_str(),
            "execution cwd must not become originating scope"
        );
    }
    assert_eq!(
        client
            .post(format!(
                "http://{original}/v1/agent-runs/{direct_id}/cancel"
            ))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        workers.read().await.get_result(&direct_id)?.status,
        SubAgentStatus::Running
    );
    let stopped: Value = client
        .post(format!(
            "http://{}/v1/agent-runs/{direct_id}/cancel",
            second_ready.endpoint
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let repeated: Value = client
        .post(format!(
            "http://{}/v1/agent-runs/{direct_id}/cancel",
            first_ready.endpoint
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        stopped["status"], repeated["status"],
        "terminal repeated cancellation stays idempotent"
    );
    assert_eq!(stopped["spec"]["worker_id"], direct_id);
    assert_eq!(
        workers
            .read()
            .await
            .rate_limit_governor()
            .snapshot(std::time::Instant::now())
            .max_capacity,
        2
    );

    let (admitted_a, admitted_b) = tokio::try_join!(
        scopes.admit(selected_workspace.clone()),
        scopes.admit(selected_workspace.clone())
    )?;
    assert!(Arc::ptr_eq(&admitted_a, &admitted_b));
    assert!(Arc::ptr_eq(&admitted_a, &selected_scope));
    assert!(!Arc::ptr_eq(
        original_scope.lsp.get().unwrap(),
        selected_scope.lsp.get().unwrap()
    ));
    let a = client
        .get(format!(
            "http://{}/v1/apps/mcp/tools?connect=true",
            first_ready.endpoint
        ))
        .send();
    let b = client
        .get(format!(
            "http://{}/v1/apps/mcp/tools?connect=true",
            second_ready.endpoint
        ))
        .send();
    let (a, b) = tokio::try_join!(a, b)?;
    a.error_for_status()?;
    b.error_for_status()?;
    client
        .get(format!("http://{original}/v1/apps/mcp/tools?connect=true"))
        .send()
        .await?
        .error_for_status()?;
    let original_pool = original_scope.mcp.lock().await.as_ref().unwrap().1.clone();
    let selected_pool = selected_scope.mcp.lock().await.as_ref().unwrap().1.clone();
    assert!(!Arc::ptr_eq(&original_pool, &selected_pool));
    assert!(Arc::ptr_eq(
        &original_pool.lock().await.dynamic_servers,
        &selected_pool.lock().await.dynamic_servers
    ));
    // A pending pool operation keeps its exact live handle while the single
    // persisted global mutation settles. No pool replacement or replay occurs.
    let pending = selected_pool.lock().await;
    let base = format!("http://{original}/v1/apps/mcp/servers");
    mcp_test_success(
        client
            .post(&base)
            .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?)
            .json(&json!({"name":"scope-proof","command":"scope-proof-never-executed"}))
            .send()
            .await?,
    )
    .await?;
    assert!(!pending.server_names().contains(&"scope-proof".to_string()));
    drop(pending);
    for (method, suffix, body) in [
        (reqwest::Method::GET, "", None),
        (
            reqwest::Method::PATCH,
            "/scope-proof",
            Some(json!({"args":["changed"]})),
        ),
        (reqwest::Method::POST, "/scope-proof/disable", None),
        (reqwest::Method::POST, "/scope-proof/enable", None),
        (reqwest::Method::DELETE, "/scope-proof", None),
    ] {
        if method != reqwest::Method::GET {
            let mut request = client
                .request(method, format!("{base}{suffix}"))
                .header(header::IF_MATCH, mcp_test_revision(&client, &base).await?);
            if let Some(body) = body {
                request = request.json(&body);
            }
            mcp_test_success(request.send().await?).await?;
        }
        for address in [original, first_ready.endpoint, second_ready.endpoint] {
            mcp_test_success(
                client
                    .get(format!("http://{address}/v1/apps/mcp/servers"))
                    .send()
                    .await?,
            )
            .await?;
        }
        let generation = scopes
            .mcp_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        for (scope, pool) in [
            (&original_scope, &original_pool),
            (&selected_scope, &selected_pool),
        ] {
            let cache = scope.mcp.lock().await;
            let (actual, retained) = cache.as_ref().unwrap();
            assert_eq!(
                *actual, generation,
                "global mutation must invalidate both workspace catalogs"
            );
            assert!(
                Arc::ptr_eq(retained, pool),
                "accepted pool identity must survive reload"
            );
        }
    }
    assert!(
        !selected_pool
            .lock()
            .await
            .server_names()
            .contains(&"scope-proof".to_string())
    );
    drop(first);
    client
        .get(format!("http://{}/v1/lsp", second_ready.endpoint))
        .send()
        .await?
        .error_for_status()?;
    assert!(Arc::ptr_eq(
        &scopes.admit(selected_workspace.clone()).await?,
        &selected_scope
    ));
    let retired_workspace = root.join("retired-workspace");
    #[cfg(windows)]
    {
        // This fixture still owns a Fleet ledger whose ancestor pins deny
        // deletion. Check that protection before releasing the test owner.
        let error = fs::rename(&selected_workspace, &retired_workspace)
            .expect_err("a live Fleet ledger must prevent workspace replacement");
        assert_eq!(error.raw_os_error(), Some(32), "{error}");
        assert!(!retired_workspace.exists());
        assert!(Arc::ptr_eq(
            &scopes.admit(selected_workspace.clone()).await?,
            &selected_scope
        ));
    }
    drop(selected_fleet);
    #[cfg(windows)]
    {
        let error = fs::rename(&selected_workspace, &retired_workspace)
            .expect_err("a live Runtime owner must prevent workspace replacement");
        assert_eq!(error.raw_os_error(), Some(5), "{error}");
        assert!(!retired_workspace.exists());
    }
    manager.shutdown_and_wait().await?;
    #[cfg(windows)]
    {
        let error = fs::rename(&selected_workspace, &retired_workspace)
            .expect_err("Engine shutdown must retain the Runtime owner's workspace lock");
        assert_eq!(error.raw_os_error(), Some(5), "{error}");
        assert!(!retired_workspace.exists());
        assert!(Arc::ptr_eq(
            &scopes.admit(selected_workspace.clone()).await?,
            &selected_scope
        ));
        client
            .get(format!("http://{}/v1/lsp", second_ready.endpoint))
            .send()
            .await?
            .error_for_status()?;
    }
    #[cfg(unix)]
    {
        fs::rename(&selected_workspace, &retired_workspace)?;
        fs::create_dir(&selected_workspace)?;
        assert!(scopes.admit(selected_workspace.clone()).await.is_err());
        assert_eq!(
            client
                .get(format!("http://{}/v1/lsp", second_ready.endpoint))
                .send()
                .await?
                .status(),
            StatusCode::CONFLICT
        );
    }
    client
        .get(format!("http://{original}/v1/lsp"))
        .send()
        .await?
        .error_for_status()?;
    // Retained scope admission is finite even after frontend detach. The
    // current owner holds caches/cleanup instead of creating an idle leak.
    for n in 2..MAX_RUNTIME_WORKSPACE_SCOPES {
        let path = root.join(format!("scope-{n}"));
        fs::create_dir(&path)?;
        scopes.admit(path).await?;
    }
    let excess = root.join("excess-scope");
    fs::create_dir(&excess)?;
    assert!(scopes.admit(excess).await.is_err());
    assert_eq!(scopes.scopes.lock().len(), MAX_RUNTIME_WORKSPACE_SCOPES);
    drop(second);
    drop((
        admitted_a,
        admitted_b,
        original_scope,
        selected_scope,
        original_pool,
        selected_pool,
        scopes,
    ));
    server.abort();
    Ok(())
}

#[tokio::test]
async fn real_owner_rejects_different_operator_source_before_attach_and_accepts_exact_alias()
-> Result<()> {
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let config_path = root.join("operator.toml");
    fs::write(&config_path, "")?;
    let config = codewhale_plugins::providers::load_config(Some(config_path.clone()), None)?;
    let other_config = root.join("other-operator.toml");
    fs::write(&other_config, "")?;
    let alias_directory = root.join("alias");
    fs::create_dir(&alias_directory)?;
    let alias = alias_directory.join("..").join("operator.toml");
    #[cfg(unix)]
    let socket = root.join("private-run").join("config-owner.sock");
    #[cfg(windows)]
    let socket = PathBuf::from(format!(r"\\.\pipe\config-owner-{}", Uuid::new_v4()));
    let token = "cwrt_config_source_private";
    let (original, manager, server) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root.clone(),
            root.join("sessions"),
            Some(token.into()),
            false,
            workspace.clone(),
            TestServerOverrides {
                config: Some(config),
                config_path: Some(config_path.clone()),
                owner_socket: Some(socket.clone()),
                ..Default::default()
            },
        )
        .await?
        .context("actual config-bound owner required")?;
    let _abort = AbortOnDrop(server.abort_handle());
    let control = codewhale_app_server::daemon_client::connect(
        Some(config_path.clone()),
        Some(socket.clone()),
    )
    .await?;
    let receipt = control.receipt().clone();
    drop(control);
    let mut wrong = listener_selection(workspace.clone(), 0, token.into());
    wrong.config_source = Some(other_config.clone());
    let error = codewhale_app_server::daemon_client::connect_listener_if_published(
        Some(config_path.clone()),
        Some(socket.clone()),
        wrong,
        receipt.clone(),
    )
    .await
    .err()
    .context("explicit other operator source must refuse before attach ACK")?;
    let message = error.to_string();
    assert!(
        message == "selected owner refused guest attachment",
        "{message}"
    );
    assert!(!message.contains(token));
    assert!(!message.contains(other_config.to_str().unwrap()));
    let mut matching = listener_selection(workspace, 0, token.into());
    matching.config_source = Some(alias);
    let mut selected = codewhale_app_server::daemon_client::connect_listener_if_published(
        Some(config_path),
        Some(socket),
        matching,
        receipt,
    )
    .await?
    .context("same canonical captured operator source must be admitted")?;
    let ready = frontend_ready(&mut selected).await?;
    let client = authenticated_http(token)?;
    client
        .get(format!("http://{}/v1/threads", ready.endpoint))
        .send()
        .await?
        .error_for_status()?;
    drop(selected);
    client
        .get(format!("http://{original}/v1/threads"))
        .send()
        .await?
        .error_for_status()?;
    manager.shutdown_and_wait().await?;
    server.abort();
    Ok(())
}

#[test]
fn captured_operator_source_preserves_uncreated_nested_selection_without_reminting() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let selected = root
        .join("not-created")
        .join("nested")
        .join("operator.toml");
    assert_eq!(
        canonical_runtime_config_source(Some(selected.clone()))?,
        Some(selected.clone())
    );
    assert!(
        !root.join("not-created").exists(),
        "selection admission must not create a config or its parents"
    );
    assert_ne!(
        canonical_runtime_config_source(Some(root.join("other.toml")))?,
        Some(selected)
    );
    Ok(())
}

#[tokio::test]
async fn canonical_history_import_runs_real_engine_retains_all_branches_and_refuses_lost_commit()
-> Result<()> {
    use codewhale_protocol::{
        CanonicalHistoryImportRequest, CanonicalThreadReceipt, LegacyThreadHistory, MessageRecord,
    };
    let _env = lock_test_env();
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let sessions_dir = root.join("sessions");
    let token = format!("history-fixture-{}", Uuid::new_v4());
    let config = Config::default().with_legacy_root(
        Some("history-fixture-key".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    let (addr, runtime, server) = spawn_test_server_with_root_token_mobile_workspace_and_overrides(
        root.clone(),
        sessions_dir.clone(),
        Some(token.clone()),
        false,
        workspace.clone(),
        TestServerOverrides {
            config: Some(config),
            ..TestServerOverrides::default()
        },
    )
    .await?
    .context("canonical history acceptance requires an actual listener")?;
    let _server = AbortOnDrop(server.abort_handle());
    let model = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "canonical successor",
    )]));
    runtime.set_test_model_client(model.clone());
    let binding = runtime.session_store_binding();
    let request = CanonicalHistoryImportRequest {
        version: 1,
        operation_key: "canonical-history-acceptance".into(),
        expected_data_dir: binding.data_dir.clone(),
        expected_execution_scope: binding.execution_scope.clone(),
        target_runtime_thread_id: None,
        workspace,
        model: Some("deepseek-v4-pro".into()),
        history: LegacyThreadHistory {
            goal: None,
            version: 1,
            state_store_id: "c".repeat(64),
            thread_id: "legacy-full-graph".into(),
            current_leaf_id: Some(3),
            messages: [
                (1, None, "system", "retained system context"),
                (2, Some(1), "user", "retained user context"),
                (3, Some(2), "assistant", "retained prior answer"),
                (4, Some(1), "user", "retained inactive branch"),
            ]
            .into_iter()
            .map(|(id, parent_entry_id, role, content)| MessageRecord {
                id,
                thread_id: "legacy-full-graph".into(),
                role: role.into(),
                content: content.into(),
                item: None,
                created_at: 1_700_000_000 + id,
                parent_entry_id,
            })
            .collect(),
        },
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    let client = codewhale_release::tls::reqwest_client_builder()
        .default_headers(headers)
        .build()?;
    let endpoint = format!("http://{addr}/v1/thread-history/import");
    let receipt: CanonicalThreadReceipt = client
        .post(&endpoint)
        .json(&request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let repeated: CanonicalThreadReceipt = client
        .post(&endpoint)
        .json(&request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        receipt, repeated,
        "replay returns the same durable canonical identity"
    );
    // Inject the precise durable crash boundary after the real owner has
    // seeded records/checkpoint but before its operation commit marker. The
    // journal retains System while the Runtime item projection omits it.
    let operation_path = binding.data_dir.join("turn-operations").join(format!(
        "history_{}.json",
        codewhale_runtime::hashing::sha256_hex(
            format!("codewhale:history-operation:v1\0{}", request.operation_key).as_bytes()
        )
    ));
    let mut operation: Value = serde_json::from_slice(&fs::read(&operation_path)?)?;
    operation["committed"] = json!(false);
    codewhale_util::utils::write_atomic(&operation_path, &serde_json::to_vec(&operation)?)?;
    let post_seed_retry: CanonicalThreadReceipt = client
        .post(&endpoint)
        .json(&request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        receipt, post_seed_retry,
        "a seeded System-containing full graph recovers without comparing lossy item projection"
    );
    let directory = sessions_dir.clone();
    let session_id = receipt.session_id.clone();
    let ticket = crate::test_support::env_scope_ticket();
    let saved = codewhale_app_server::daemon_socket::owner_work(move || {
        let _scope = crate::test_support::join_env_scope(ticket);
        Ok(SessionManager::new(directory)?.load_session_snapshot(&session_id)?)
    })
    .await?;
    assert_eq!(
        saved
            .journal
            .as_ref()
            .context("full imported journal")?
            .entries
            .len(),
        4
    );
    assert_eq!(saved.messages.len(), 3);
    assert_eq!(saved.metadata.runtime_store.as_ref(), Some(&binding));
    let full: codewhale_protocol::CanonicalThreadSnapshot = client
        .get(format!(
            "http://{addr}/v1/threads/{}/history",
            receipt.runtime_thread_id
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        full.saved_session_id.as_deref(),
        Some(receipt.session_id.as_str())
    );
    assert_eq!(full.data_dir, binding.data_dir);
    let full: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(full.session)?;
    assert_eq!(
        full.journal
            .as_ref()
            .context("complete read journal")?
            .entries
            .len(),
        4
    );
    assert_eq!(full.messages, saved.messages);
    let mut changed = request.clone();
    changed.history.messages[0].content = "changed operation input".into();
    assert_eq!(
        client.post(&endpoint).json(&changed).send().await?.status(),
        StatusCode::CONFLICT
    );
    let started: Value = client
        .post(format!(
            "http://{addr}/v1/threads/{}/turns",
            receipt.runtime_thread_id
        ))
        .json(&json!({"prompt":"continue canonical history"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = started["turn"]["id"].as_str().context("actual turn id")?;
    assert_eq!(
        wait_for_terminal_turn_status(
            &client,
            addr,
            &receipt.runtime_thread_id,
            turn_id,
            Duration::from_secs(30)
        )
        .await?,
        "completed"
    );
    let actual = serde_json::to_string(&model.captured_requests())?;
    assert!(actual.contains("retained user context"));
    assert!(actual.contains("retained prior answer"));
    assert!(
        !actual.contains("retained inactive branch"),
        "the inactive branch remains saved but is not the active provider transcript"
    );
    let repeated: CanonicalThreadReceipt = client
        .post(&endpoint)
        .json(&request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        receipt, repeated,
        "a legitimate successor does not remint the import"
    );
    let full: codewhale_protocol::CanonicalThreadSnapshot = client
        .get(format!(
            "http://{addr}/v1/threads/{}/history",
            receipt.runtime_thread_id
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let full: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(full.session)?;
    assert!(serde_json::to_string(&full)?.contains("retained inactive branch"));
    assert!(serde_json::to_string(&full.messages)?.contains("canonical successor"));
    // A pre-existing bare compatibility link adopts into this actual bound
    // canonical thread; its current branch and successor remain authoritative.
    let mut linked = request.clone();
    linked.operation_key = "canonical-existing-link-adoption".into();
    linked.target_runtime_thread_id = Some(receipt.runtime_thread_id.clone());
    linked.history.state_store_id = "d".repeat(64);
    linked.history.messages[1].content = "legacy linked branch evidence".into();
    let adopted: CanonicalThreadReceipt = client
        .post(&endpoint)
        .json(&linked)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(adopted.runtime_thread_id, receipt.runtime_thread_id);
    assert_eq!(adopted.session_id, receipt.session_id);
    let repeated: CanonicalThreadReceipt = client
        .post(&endpoint)
        .json(&linked)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(adopted, repeated);
    let full: codewhale_protocol::CanonicalThreadSnapshot = client
        .get(format!(
            "http://{addr}/v1/threads/{}/history",
            receipt.runtime_thread_id
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let full: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(full.session)?;
    assert!(serde_json::to_string(&full.journal)?.contains("legacy linked branch evidence"));
    assert!(!serde_json::to_string(&full.messages)?.contains("legacy linked branch evidence"));
    assert!(serde_json::to_string(&full.messages)?.contains("canonical successor"));
    let directory = sessions_dir;
    let session_id = receipt.session_id.clone();
    let ticket = crate::test_support::env_scope_ticket();
    codewhale_app_server::daemon_socket::owner_work(move || {
        let _scope = crate::test_support::join_env_scope(ticket);
        let manager = SessionManager::new(directory.clone())?;
        let _lease = manager.reserve_session_for_external_write(&session_id)?;
        fs::remove_file(directory.join(format!("{session_id}.json")))?;
        Ok(())
    })
    .await?;
    assert_eq!(
        client.post(&endpoint).json(&request).send().await?.status(),
        StatusCode::CONFLICT,
        "a missing committed full document is recovery, never a fresh import"
    );
    assert_eq!(
        client
            .get(format!(
                "http://{addr}/v1/threads/{}/history",
                receipt.runtime_thread_id
            ))
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT,
        "full read refuses the missing authoritative document too"
    );
    assert_eq!(
        runtime
            .list_threads(
                codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
                None
            )
            .await?
            .len(),
        1
    );
    Ok(())
}

/// Actual authenticated owner listener plus real Runtime/Engine; only the
/// provider response is scripted. Each test owns its isolated HOME and store.
struct HistoryOperationFixture {
    addr: SocketAddr,
    runtime: Arc<RuntimeThreadManager>,
    client: reqwest::Client,
    sessions_dir: PathBuf,
    workspace: PathBuf,
    model: Arc<MockLlmClient>,
    _server: AbortOnDrop,
}
impl HistoryOperationFixture {
    async fn new(root: &FsPath) -> Result<Self> {
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace)?;
        let sessions_dir = root.join("sessions");
        let token = format!("history-operation-fixture-{}", Uuid::new_v4());
        let config = Config::default().with_legacy_root(
            Some("history-fixture-key".into()),
            Some("http://127.0.0.1:1/v1".into()),
        );
        let (addr, runtime, server) =
            spawn_test_server_with_root_token_mobile_workspace_and_overrides(
                root.to_path_buf(),
                sessions_dir.clone(),
                Some(token.clone()),
                false,
                workspace.clone(),
                TestServerOverrides {
                    config: Some(config),
                    ..Default::default()
                },
            )
            .await?
            .context("history acceptance requires an actual owner listener")?;
        let model = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "actual successor",
        )]));
        runtime.set_test_model_client(model.clone());
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse()?,
        );
        Ok(Self {
            addr,
            runtime,
            client: codewhale_release::tls::reqwest_client_builder()
                .default_headers(headers)
                .build()?,
            sessions_dir,
            workspace,
            model,
            _server: AbortOnDrop(server.abort_handle()),
        })
    }
    fn url(&self, suffix: &str) -> String {
        format!("http://{}{suffix}", self.addr)
    }
    fn mutation(
        &self,
        key: &str,
        mutation: codewhale_protocol::CanonicalThreadMutation,
    ) -> codewhale_protocol::CanonicalThreadMutationRequest {
        let binding = self.runtime.session_store_binding();
        codewhale_protocol::CanonicalThreadMutationRequest {
            version: 1,
            operation_key: key.into(),
            expected_data_dir: binding.data_dir,
            expected_execution_scope: binding.execution_scope,
            workspace: self.workspace.clone(),
            mutation,
        }
    }
    async fn submit(
        &self,
        request: &codewhale_protocol::CanonicalThreadMutationRequest,
    ) -> Result<codewhale_protocol::CanonicalThreadReceipt> {
        Ok(self
            .client
            .post(self.url("/v1/thread-history/mutate"))
            .json(request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    async fn snapshot(&self, id: &str) -> Result<codewhale_protocol::CanonicalThreadSnapshot> {
        Ok(self
            .client
            .get(self.url(&format!("/v1/threads/{id}/history")))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    async fn lookup(
        &self,
        request: &codewhale_protocol::CanonicalThreadMutationRequest,
    ) -> Result<codewhale_protocol::CanonicalThreadOperationStatus> {
        Ok(self
            .client
            .post(self.url("/v1/thread-history/operations/lookup"))
            .json(&codewhale_protocol::CanonicalThreadOperationLookup {
                version: request.version,
                operation_key: request.operation_key.clone(),
                expected_data_dir: request.expected_data_dir.clone(),
                expected_execution_scope: request.expected_execution_scope.clone(),
                workspace: request.workspace.clone(),
            })
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    async fn import_branched_source(&self) -> Result<codewhale_protocol::CanonicalThreadReceipt> {
        self.import_branched_source_with_goal(None).await
    }
    async fn import_branched_source_with_goal(
        &self,
        goal: Option<codewhale_protocol::ThreadGoal>,
    ) -> Result<codewhale_protocol::CanonicalThreadReceipt> {
        let binding = self.runtime.session_store_binding();
        let request = codewhale_protocol::CanonicalHistoryImportRequest {
            version: 1,
            operation_key: "operation-source-full-branches".into(),
            expected_data_dir: binding.data_dir,
            expected_execution_scope: binding.execution_scope,
            target_runtime_thread_id: None,
            workspace: self.workspace.clone(),
            model: Some("deepseek-v4-pro".into()),
            history: codewhale_protocol::LegacyThreadHistory {
                goal,
                version: 1,
                state_store_id: "f".repeat(64),
                thread_id: "original-operation-source".into(),
                current_leaf_id: Some(3),
                messages: [
                    (1, None, "system", "system retained"),
                    (2, Some(1), "user", "original active prompt"),
                    (3, Some(2), "assistant", "original active answer"),
                    (4, Some(1), "user", "inactive branch selected for fork"),
                ]
                .into_iter()
                .map(
                    |(id, parent_entry_id, role, content)| codewhale_protocol::MessageRecord {
                        id,
                        thread_id: "original-operation-source".into(),
                        role: role.into(),
                        content: content.into(),
                        item: None,
                        created_at: 1_700_000_000 + id,
                        parent_entry_id,
                    },
                )
                .collect(),
            },
        };
        Ok(self
            .client
            .post(self.url("/v1/thread-history/import"))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[tokio::test]
async fn canonical_create_commits_one_identity_and_read_only_lookup_has_no_turn_effect()
-> Result<()> {
    use codewhale_protocol::{CanonicalThreadMutation, CanonicalThreadOperationStatus};
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    let request = fixture.mutation(
        "actual-create-intent",
        CanonicalThreadMutation::Create {
            config: json!({"model":"deepseek-v4-pro","allow_shell":false}),
        },
    );
    assert!(matches!(
        fixture.lookup(&request).await?,
        CanonicalThreadOperationStatus::Absent
    ));
    let receipt = fixture.submit(&request).await?;
    assert_eq!(fixture.submit(&request).await?, receipt);
    match fixture.lookup(&request).await? {
        CanonicalThreadOperationStatus::Committed {
            receipt: recovered,
            association,
        } => {
            assert_eq!(receipt, recovered);
            assert_eq!(
                association.kind,
                codewhale_protocol::CanonicalThreadOperationKind::Create
            );
            assert!(association.source_runtime_thread_id.is_none());
        }
        other => anyhow::bail!("unexpected settled lookup: {other:?}"),
    }
    let snapshot = fixture.snapshot(&receipt.runtime_thread_id).await?;
    let again = fixture.snapshot(&receipt.runtime_thread_id).await?;
    assert_eq!(
        snapshot.document_digest, again.document_digest,
        "read-only projection is stable"
    );
    assert_eq!(snapshot.session, again.session);
    assert_eq!(
        fixture
            .runtime
            .list_threads(
                codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
                None
            )
            .await?
            .len(),
        1
    );
    assert!(
        fixture
            .runtime
            .get_thread_detail(&receipt.runtime_thread_id)
            .await?
            .turns
            .is_empty()
    );
    assert!(fixture.model.captured_requests().is_empty());
    let mut changed = request.clone();
    changed.mutation = CanonicalThreadMutation::Create {
        config: json!({"model":"deepseek-v4-pro","allow_shell":true}),
    };
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/mutate"))
            .json(&changed)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    let mut wrong_scope = codewhale_protocol::CanonicalThreadOperationLookup {
        version: 1,
        operation_key: request.operation_key,
        expected_data_dir: request.expected_data_dir,
        expected_execution_scope: request.expected_execution_scope,
        workspace: root.join("another-scope"),
    };
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/lookup"))
            .json(&wrong_scope)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    wrong_scope.workspace = fixture.workspace.clone();
    wrong_scope.expected_execution_scope = "another-owner".into();
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/lookup"))
            .json(&wrong_scope)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    Ok(())
}

#[tokio::test]
async fn canonical_fork_keeps_all_branches_selects_exact_leaf_and_recovers_after_successor()
-> Result<()> {
    use codewhale_protocol::{
        CanonicalHistoryOptions, CanonicalHistorySource, CanonicalThreadMutation,
        CanonicalThreadOperationStatus,
    };
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    let original = fixture.import_branched_source().await?;
    let snapshot = fixture.snapshot(&original.runtime_thread_id).await?;
    let source: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(snapshot.session.clone())?;
    let selected = source.journal.as_ref().context("source graph")?.entries[3]
        .id
        .clone();
    let mut request = fixture.mutation(
        "actual-fork-intent",
        CanonicalThreadMutation::Fork {
            source: CanonicalHistorySource::Thread {
                runtime_thread_id: original.runtime_thread_id.clone(),
                expected_document_digest: snapshot.document_digest,
            },
            options: CanonicalHistoryOptions::default(),
            selected_entry_id: Some(selected.clone()),
        },
    );
    // A selected target root is independent of the source root; the source is unchanged.
    request.workspace = root.join("fork-workspace");
    fs::create_dir_all(&request.workspace)?;
    let receipt = fixture.submit(&request).await?;
    assert_ne!(original.session_id, receipt.session_id);
    assert_ne!(original.runtime_thread_id, receipt.runtime_thread_id);
    // The selected branch has two messages, unlike the three-message source.
    // Inspect raw publication before the protected loader can restore its count.
    let raw: codewhale_engine::session_manager::SavedSession = serde_json::from_slice(&fs::read(
        fixture
            .sessions_dir
            .join(format!("{}.json", receipt.session_id)),
    )?)?;
    assert_eq!(raw.metadata.message_count, 2);
    assert_eq!(raw.messages.len(), 2);
    assert_ne!(raw.metadata.message_count, source.metadata.message_count);
    let prepared_graph = raw.journal.as_ref().context("raw selected full graph")?;
    assert_eq!(
        prepared_graph.entries,
        source.journal.as_ref().unwrap().entries
    );
    assert_eq!(prepared_graph.leaf_id.as_deref(), Some(selected.as_str()));
    assert_eq!(raw.leaf_id, prepared_graph.leaf_id);
    assert_eq!(raw.messages, prepared_graph.to_messages());
    let operation_path = request
        .expected_data_dir
        .join("turn-operations")
        .join(format!(
            "history_{}.json",
            codewhale_runtime::hashing::sha256_hex(
                format!("codewhale:history-operation:v1\0{}", request.operation_key).as_bytes()
            )
        ));
    let mut prepared: Value = serde_json::from_slice(&fs::read(&operation_path)?)?;
    assert_eq!(
        prepared["target_document_digest"],
        json!(codewhale_engine::runtime_threads::manager::history::saved_document_digest(&raw)?),
        "Fork recovery binds the exact shorter branch count published to disk"
    );
    // Model interruption after complete publication but before the intent commit.
    prepared["committed"] = json!(false);
    codewhale_util::utils::write_atomic(&operation_path, &serde_json::to_vec(&prepared)?)?;
    let association = match fixture.lookup(&request).await? {
        CanonicalThreadOperationStatus::Pending { association, .. } => association,
        other => anyhow::bail!("expected prepared Fork intent: {other:?}"),
    };
    let recovery = codewhale_protocol::CanonicalThreadOperationRecovery {
        operation: codewhale_protocol::CanonicalThreadOperationLookup {
            version: request.version,
            operation_key: request.operation_key.clone(),
            expected_data_dir: request.expected_data_dir.clone(),
            expected_execution_scope: request.expected_execution_scope.clone(),
            workspace: request.workspace.clone(),
        },
        association,
    };
    let prepared_detail = fixture
        .runtime
        .get_thread_detail(&receipt.runtime_thread_id)
        .await?;
    for _ in 0..2 {
        let recovered: CanonicalThreadOperationStatus = fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/recover"))
            .json(&recovery)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match recovered {
            CanonicalThreadOperationStatus::Committed {
                receipt: settled,
                association,
            } => {
                assert_eq!(settled, receipt);
                assert_eq!(association, recovery.association);
            }
            other => anyhow::bail!("prepared exact-key Fork recovery did not settle: {other:?}"),
        }
    }
    let recovered_detail = fixture
        .runtime
        .get_thread_detail(&receipt.runtime_thread_id)
        .await?;
    assert_eq!(prepared_detail.turns.len(), recovered_detail.turns.len());
    assert_eq!(prepared_detail.items.len(), recovered_detail.items.len());
    assert!(fixture.model.captured_requests().is_empty());
    let fork = fixture.snapshot(&receipt.runtime_thread_id).await?;
    let fork: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(fork.session)?;
    assert_eq!(
        fork.journal.as_ref().context("fork graph")?.entries,
        source.journal.as_ref().unwrap().entries,
        "inactive and active branches remain complete"
    );
    assert_eq!(fork.leaf_id.as_deref(), Some(selected.as_str()));
    assert_eq!(
        fork.metadata.parent_session_id.as_deref(),
        Some(original.session_id.as_str())
    );
    assert_eq!(fork.metadata.workspace, request.workspace);
    assert_eq!(
        fixture.snapshot(&original.runtime_thread_id).await?.session,
        snapshot.session,
        "fork never rewrites source"
    );
    let started: Value = fixture.client.post(fixture.url(&format!("/v1/threads/{}/turns",receipt.runtime_thread_id)))
        .json(&json!({"prompt":"continue selected branch","operation_key":"fork-successor","expected_workspace":request.workspace})).send().await?.error_for_status()?.json().await?;
    let turn_id = started["turn"]["id"]
        .as_str()
        .context("actual successor turn")?;
    assert_eq!(
        wait_for_terminal_turn_status(
            &fixture.client,
            fixture.addr,
            &receipt.runtime_thread_id,
            turn_id,
            Duration::from_secs(30)
        )
        .await?,
        "completed"
    );
    let requests = serde_json::to_string(&fixture.model.captured_requests())?;
    assert!(requests.contains("inactive branch selected for fork"));
    assert!(!requests.contains("original active answer"));
    let before = fixture
        .runtime
        .get_thread_detail(&receipt.runtime_thread_id)
        .await?;
    let recovered = fixture.lookup(&request).await?;
    match recovered {
        CanonicalThreadOperationStatus::Committed {
            receipt: known,
            association,
        } => {
            assert_eq!(known, receipt);
            assert_eq!(
                association.kind,
                codewhale_protocol::CanonicalThreadOperationKind::Fork
            );
            assert_eq!(
                association.source_session_id.as_deref(),
                Some(original.session_id.as_str())
            );
        }
        other => anyhow::bail!("unexpected successor recovery: {other:?}"),
    }
    let after = fixture
        .runtime
        .get_thread_detail(&receipt.runtime_thread_id)
        .await?;
    assert_eq!(before.turns.len(), after.turns.len());
    assert_eq!(before.items.len(), after.items.len());
    let mut refreshed = request.clone();
    if let CanonicalThreadMutation::Fork {
        source:
            CanonicalHistorySource::Thread {
                expected_document_digest,
                ..
            },
        ..
    } = &mut refreshed.mutation
    {
        *expected_document_digest = "0".repeat(64);
    }
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/mutate"))
            .json(&refreshed)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT,
        "read-only lookup, rather than a reminted source proposal, recovers the retained key"
    );
    Ok(())
}

#[tokio::test]
async fn canonical_resume_offered_suffix_settles_once_and_full_graph_witness_refuses_tamper()
-> Result<()> {
    use codewhale_protocol::{
        CanonicalHistoryOptions, CanonicalHistorySource, CanonicalThreadMutation,
        CanonicalThreadOperationStatus,
    };
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    let original = fixture.import_branched_source().await?;
    let snapshot = fixture.snapshot(&original.runtime_thread_id).await?;
    let source: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(snapshot.session)?;
    let mut offered = source
        .messages
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    offered.push(serde_json::to_value(codewhale_models::Message {
        role: codewhale_models::Role::User,
        content: vec![codewhale_models::ContentBlock::Text {
            text: "genuine new offered suffix".into(),
            cache_control: None,
        }],
    })?);
    let request = fixture.mutation(
        "actual-resume-intent",
        CanonicalThreadMutation::Resume {
            source: CanonicalHistorySource::Thread {
                runtime_thread_id: original.runtime_thread_id.clone(),
                expected_document_digest: snapshot.document_digest,
            },
            options: CanonicalHistoryOptions {
                expected_session_goal_digest: None,
                offered_history: offered,
                overrides: json!({"base_instructions":"keep canonical receipt"}),
                source_path: Some(
                    fixture
                        .sessions_dir
                        .join(format!("{}.json", original.session_id)),
                ),
            },
        },
    );
    let receipt = fixture.submit(&request).await?;
    assert_eq!(receipt.runtime_thread_id, original.runtime_thread_id);
    assert_eq!(receipt.session_id, original.session_id);
    // Inspect the exact publication before a protected loader can normalize it.
    let raw: codewhale_engine::session_manager::SavedSession = serde_json::from_slice(&fs::read(
        fixture
            .sessions_dir
            .join(format!("{}.json", receipt.session_id)),
    )?)?;
    assert_eq!(raw.metadata.message_count, 4);
    assert_eq!(raw.messages.len(), 4);
    let published_journal = raw.journal.as_ref().context("published full journal")?;
    let offered_leaf = format!(
        "offered:{}:3",
        codewhale_runtime::hashing::sha256_hex(request.operation_key.as_bytes())
    );
    assert_eq!(
        published_journal.leaf_id.as_deref(),
        Some(offered_leaf.as_str())
    );
    assert_eq!(raw.leaf_id, published_journal.leaf_id);
    assert_eq!(raw.messages, published_journal.to_messages());
    let before = fixture
        .runtime
        .get_thread_detail(&receipt.runtime_thread_id)
        .await?;
    assert_eq!(
        before.turns.len(),
        2,
        "only the non-overlapping offered user suffix is seeded"
    );
    let full = fixture.snapshot(&receipt.runtime_thread_id).await?;
    let full: codewhale_engine::session_manager::SavedSession =
        serde_json::from_value(full.session)?;
    assert_eq!(full.journal.as_ref().unwrap().entries.len(), 5);
    assert_eq!(full.messages.len(), 4);
    assert!(
        full.system_prompt
            .as_deref()
            .is_some_and(|text| text.contains("keep canonical receipt"))
    );
    // Actual completed seed/checkpoint, then emulate interruption before the intent marker.
    let operation_path = request
        .expected_data_dir
        .join("turn-operations")
        .join(format!(
            "history_{}.json",
            codewhale_runtime::hashing::sha256_hex(
                format!("codewhale:history-operation:v1\0{}", request.operation_key).as_bytes()
            )
        ));
    let mut operation: Value = serde_json::from_slice(&fs::read(&operation_path)?)?;
    assert_eq!(
        operation["target_document_digest"],
        json!(codewhale_engine::runtime_threads::manager::history::saved_document_digest(&raw)?),
        "the retained recovery witness must bind the exact count and offered leaf published to disk"
    );
    operation["committed"] = json!(false);
    codewhale_util::utils::write_atomic(&operation_path, &serde_json::to_vec(&operation)?)?;
    let association = match fixture.lookup(&request).await? {
        CanonicalThreadOperationStatus::Pending { association, .. } => association,
        other => anyhow::bail!("expected published pending intent: {other:?}"),
    };
    let recovery = codewhale_protocol::CanonicalThreadOperationRecovery {
        operation: codewhale_protocol::CanonicalThreadOperationLookup {
            version: request.version,
            operation_key: request.operation_key.clone(),
            expected_data_dir: request.expected_data_dir.clone(),
            expected_execution_scope: request.expected_execution_scope.clone(),
            workspace: request.workspace.clone(),
        },
        association,
    };
    // Reconstructing a proposal from the changed current source is not the
    // original intent. Exact-key recovery needs neither that proposal nor a
    // second source body, and must not remint its already-published session.
    let changed = fixture.snapshot(&receipt.runtime_thread_id).await?;
    let mut refreshed = request.clone();
    let CanonicalThreadMutation::Resume {
        source:
            CanonicalHistorySource::Thread {
                expected_document_digest,
                ..
            },
        ..
    } = &mut refreshed.mutation
    else {
        anyhow::bail!("Resume fixture changed");
    };
    *expected_document_digest = changed.document_digest;
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/mutate"))
            .json(&refreshed)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    let mut wrong = recovery.clone();
    wrong.association.kind = codewhale_protocol::CanonicalThreadOperationKind::Fork;
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/recover"))
            .json(&wrong)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    let mut wrong = recovery.clone();
    wrong.operation.workspace = root.join("different-selection");
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/recover"))
            .json(&wrong)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT
    );
    for _ in 0..2 {
        let recovered: CanonicalThreadOperationStatus = fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/recover"))
            .json(&recovery)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match recovered {
            CanonicalThreadOperationStatus::Committed {
                receipt: settled,
                association,
            } => {
                assert_eq!(settled, receipt);
                assert_eq!(association, recovery.association);
            }
            other => anyhow::bail!("prepared exact-key recovery did not settle: {other:?}"),
        }
    }
    assert_eq!(fixture.submit(&request).await?, receipt);
    let after = fixture
        .runtime
        .get_thread_detail(&receipt.runtime_thread_id)
        .await?;
    assert_eq!(before.turns.len(), after.turns.len());
    assert_eq!(before.items.len(), after.items.len());
    assert!(
        fixture.model.captured_requests().is_empty(),
        "history admission and recovery never execute a provider turn"
    );
    let file = fixture
        .sessions_dir
        .join(format!("{}.json", receipt.session_id));
    let mut tampered: codewhale_engine::session_manager::SavedSession =
        serde_json::from_slice(&fs::read(&file)?)?;
    let entry = &mut tampered
        .journal
        .as_mut()
        .context("tampered full graph")?
        .entries[3];
    let codewhale_runtime::session_tree::SessionEntryKind::Message { message } =
        std::sync::Arc::make_mut(&mut entry.kind)
    else {
        anyhow::bail!("inactive fixture entry lost its message");
    };
    let codewhale_models::ContentBlock::Text { text, .. } = &mut message.content[0] else {
        anyhow::bail!("inactive fixture text missing");
    };
    *text = "tampered inactive branch".into();
    codewhale_util::utils::write_atomic(&file, &serde_json::to_vec(&tampered)?)?;
    let lookup = codewhale_protocol::CanonicalThreadOperationLookup {
        version: 1,
        operation_key: request.operation_key,
        expected_data_dir: request.expected_data_dir,
        expected_execution_scope: request.expected_execution_scope,
        workspace: request.workspace,
    };
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/lookup"))
            .json(&lookup)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT,
        "matching entry count/selected leaf cannot hide a changed inactive branch"
    );
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/operations/recover"))
            .json(&recovery)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT,
        "exact-key recovery cannot conceal a changed inactive branch"
    );
    Ok(())
}

#[tokio::test]
async fn canonical_import_preserves_paused_goal_and_retry_never_overwrites_later_goal() -> Result<()>
{
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    let source = codewhale_protocol::ThreadGoal {
        thread_id: "original-operation-source".into(),
        goal_id: Uuid::new_v4().to_string(),
        objective: "preserve the actual prior goal".into(),
        status: codewhale_protocol::ThreadGoalStatus::Active,
        token_budget: Some(1000),
        tokens_used: 41,
        time_used_seconds: 9,
        continuation_count: 2,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_010,
        last_gap_fingerprint: None,
        repeated_gap_count: 0,
        last_gap_pass: None,
        pause_reason: None,
    };
    let receipt = fixture
        .import_branched_source_with_goal(Some(source.clone()))
        .await?;
    let goal = fixture
        .runtime
        .get_goal(&receipt.runtime_thread_id)
        .await?
        .context("imported actual owner goal")?;
    let mut expected = source.clone();
    expected.thread_id = receipt.runtime_thread_id.clone();
    expected.status = codewhale_protocol::ThreadGoalStatus::Paused;
    assert_eq!(
        goal, expected,
        "source counters/timestamps persist without authorizing provider work"
    );
    assert!(fixture.model.captured_requests().is_empty());
    let mut later = goal;
    later.status = codewhale_protocol::ThreadGoalStatus::Complete;
    later.updated_at += 7;
    later.tokens_used += 4;
    fixture.runtime.save_goal(later.clone()).await?;
    assert_eq!(
        fixture
            .import_branched_source_with_goal(Some(source.clone()))
            .await?,
        receipt
    );
    assert_eq!(
        fixture.runtime.get_goal(&receipt.runtime_thread_id).await?,
        Some(later)
    );
    let mut conflicting = source;
    conflicting.thread_id = "wrong-source".into();
    assert!(
        fixture
            .import_branched_source_with_goal(Some(conflicting))
            .await
            .is_err()
    );
    assert!(fixture.model.captured_requests().is_empty());
    Ok(())
}

#[tokio::test]
async fn canonical_fork_copies_local_goal_under_source_witness_and_never_replays_evolution()
-> Result<()> {
    use codewhale_engine::session_manager::{SessionGoalState, SessionGoalStatus, SessionManager};
    use codewhale_protocol::{CanonicalHistorySource, CanonicalThreadMutation};
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    let original = fixture.import_branched_source().await?;
    let manager = SessionManager::new(fixture.sessions_dir.clone())?;
    let goal: SessionGoalState = serde_json::from_value(json!({
        "objective":"preserve local goal without launching work", "status":"active",
        "token_budget":1000,"tokens_used":13,"time_used_seconds":8,
        "continuation_count":2,"elapsed_seconds":9,"goal_id":"source-local-goal",
        "last_gap_fingerprint":"a".repeat(64),"repeated_gap_count":1,"last_gap_pass":1
    }))?;
    manager.save_session_goal(&original.session_id, Some(&goal))?;
    let snapshot = fixture.snapshot(&original.runtime_thread_id).await?;
    let request = fixture.mutation(
        "local-goal-fork",
        CanonicalThreadMutation::Fork {
            source: CanonicalHistorySource::Thread {
                runtime_thread_id: original.runtime_thread_id.clone(),
                expected_document_digest: snapshot.document_digest,
            },
            options: Default::default(),
            selected_entry_id: None,
        },
    );
    let receipt = fixture.submit(&request).await?;
    let copied = manager
        .load_session_goal(&receipt.session_id)?
        .context("fork local goal sidecar")?;
    let mut paused = goal.clone();
    paused.status = SessionGoalStatus::Paused;
    paused.pause_reason = None;
    assert_eq!(copied, paused);
    assert_eq!(
        manager.load_session_goal(&original.session_id)?,
        Some(goal.clone())
    );
    let mut evolved = copied;
    evolved.status = SessionGoalStatus::Complete;
    evolved.tokens_used += 7;
    manager.save_session_goal(&receipt.session_id, Some(&evolved))?;
    assert_eq!(fixture.submit(&request).await?, receipt);
    assert_eq!(
        manager.load_session_goal(&receipt.session_id)?,
        Some(evolved)
    );
    assert!(fixture.model.captured_requests().is_empty());

    let stale = fixture.snapshot(&original.runtime_thread_id).await?;
    let changed_request = fixture.mutation(
        "changed-source-goal",
        CanonicalThreadMutation::Fork {
            source: CanonicalHistorySource::Thread {
                runtime_thread_id: original.runtime_thread_id.clone(),
                expected_document_digest: stale.document_digest,
            },
            options: Default::default(),
            selected_entry_id: None,
        },
    );
    manager.save_session_goal(&original.session_id, None)?;
    assert_eq!(
        fixture
            .client
            .post(fixture.url("/v1/thread-history/mutate"))
            .json(&changed_request)
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT,
        "missing previously captured sidecar is a changed source, never an empty inherited goal"
    );
    let goal_path = fixture
        .sessions_dir
        .join(".goals")
        .join(format!("{}.json", original.session_id));
    codewhale_util::utils::write_atomic(&goal_path, b"{corrupt-local-goal")?;
    assert_eq!(
        fixture
            .client
            .get(fixture.url(&format!(
                "/v1/threads/{}/history",
                original.runtime_thread_id
            )))
            .send()
            .await?
            .status(),
        StatusCode::CONFLICT,
        "corrupt sidecar remains visible recovery evidence"
    );
    Ok(())
}

#[tokio::test]
async fn native_fork_routes_keep_full_graph_local_goal_and_captured_session_directory() -> Result<()>
{
    use codewhale_engine::session_manager::{
        SavedSession, SessionGoalState, SessionGoalStatus, SessionManager,
    };
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    assert_ne!(
        fixture.sessions_dir,
        codewhale_engine::session_manager::default_sessions_dir()?
    );
    let original = fixture.import_branched_source().await?;
    let manager = SessionManager::new(fixture.sessions_dir.clone())?;
    let mut source = manager.load_session_snapshot_bounded(
        &original.session_id,
        codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
    )?;
    source.window_title = Some("captured native source window".into());
    source.work_state = Some(codewhale_engine::session_manager::SessionWorkState::default());
    source.metadata.cost.session_cost_usd = 1.25;
    manager.save_session(&source)?;
    let source_bytes = fs::read(
        fixture
            .sessions_dir
            .join(format!("{}.json", original.session_id)),
    )?;
    let goal: SessionGoalState = serde_json::from_value(json!({
        "objective":"native sidecar continuity", "status":"active", "token_budget":900,
        "tokens_used":17,"time_used_seconds":4,"continuation_count":2,"elapsed_seconds":5,
        "goal_id":"native-goal-revision","last_gap_fingerprint":"b".repeat(64),
        "repeated_gap_count":1,"last_gap_pass":1
    }))?;
    manager.save_session_goal(&original.session_id, Some(&goal))?;
    let source_entries = source
        .journal
        .as_ref()
        .context("native source journal")?
        .entries
        .clone();
    let source_detail = fixture
        .runtime
        .get_thread_detail(&original.runtime_thread_id)
        .await?;
    let turn_id = source_detail
        .turns
        .first()
        .context("seeded native turn")?
        .id
        .clone();
    for (suffix, body, empty) in [
        ("fork", None, false),
        ("fork-at-turn", Some(json!({"turn_id":turn_id})), false),
        ("undo", Some(json!({"depth":0})), true),
    ] {
        let request = fixture.client.post(fixture.url(&format!(
            "/v1/threads/{}/{suffix}",
            original.runtime_thread_id
        )));
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        let response: Value = request.send().await?.error_for_status()?.json().await?;
        let fork: codewhale_engine::runtime_threads::ThreadRecord =
            serde_json::from_value(if suffix == "fork" {
                response
            } else {
                response["thread"].clone()
            })?;
        assert_ne!(fork.session_id, Some(original.session_id.clone()));
        let id = fork
            .session_id
            .as_ref()
            .context("native fork owns a session")?;
        let saved: SavedSession = manager
            .load_session_snapshot_bounded(id, codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES)?;
        let graph = saved.journal.as_ref().context("full native fork journal")?;
        assert!(
            source_entries
                .iter()
                .all(|entry| graph.entries.contains(entry)),
            "every inactive branch and original entry survives {suffix}"
        );
        assert_eq!(saved.messages.is_empty(), empty);
        assert_eq!(saved.window_title, source.window_title);
        assert_eq!(saved.work_state, source.work_state);
        assert_eq!(
            serde_json::to_value(&saved.metadata.cost)?,
            serde_json::to_value(&source.metadata.cost)?
        );
        assert_eq!(
            saved.metadata.parent_session_id.as_deref(),
            Some(original.session_id.as_str())
        );
        assert_eq!(
            saved.metadata.runtime_store,
            Some(fixture.runtime.session_store_binding())
        );
        let mut paused = goal.clone();
        paused.status = SessionGoalStatus::Paused;
        paused.pause_reason = None;
        assert_eq!(manager.load_session_goal(id)?, Some(paused));
        assert!(
            fixture.runtime.get_goal(&fork.id).await?.is_none(),
            "local goal continuity does not invent public Runtime goal inheritance"
        );
    }
    assert_eq!(
        fs::read(
            fixture
                .sessions_dir
                .join(format!("{}.json", original.session_id))
        )?,
        source_bytes
    );
    assert_eq!(manager.load_session_goal(&original.session_id)?, Some(goal));
    assert!(fixture.model.captured_requests().is_empty());
    Ok(())
}

#[tokio::test]
async fn native_fork_copy_refusal_never_publishes_a_shared_session_thread() -> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
    let fixture = HistoryOperationFixture::new(&root).await?;
    let original = fixture.import_branched_source().await?;
    let before = fixture
        .runtime
        .list_threads(
            codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
            None,
        )
        .await?;
    let path = fixture
        .sessions_dir
        .join(format!("{}.json", original.session_id));
    let original_bytes = fs::read(&path)?;
    codewhale_util::utils::write_atomic(&path, b"{invalid-complete-source")?;
    let refused = fixture
        .client
        .post(fixture.url(&format!("/v1/threads/{}/fork", original.runtime_thread_id)))
        .send()
        .await?;
    assert!(!refused.status().is_success());
    let after = fixture
        .runtime
        .list_threads(
            codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
            None,
        )
        .await?;
    assert_eq!(before.len(), after.len());
    assert_eq!(
        fixture
            .runtime
            .get_thread(&original.runtime_thread_id)
            .await?
            .session_id,
        Some(original.session_id.clone())
    );
    assert_eq!(fs::read(&path)?, b"{invalid-complete-source");
    codewhale_util::utils::write_atomic(&path, &original_bytes)?;
    assert!(fixture.model.captured_requests().is_empty());
    Ok(())
}
