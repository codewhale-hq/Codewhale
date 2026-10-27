use super::*;
use crate::command_catalog::{BuiltinCommand, CommandCatalog, UserCommand};

fn builtin(name: &str, aliases: Vec<String>) -> BuiltinCommand {
    BuiltinCommand {
        name: name.to_string(),
        aliases,
        usage: format!("/{name} [value]"),
        description_id: codewhale_localization::MessageId::CmdModelDescription,
        subcommands: Vec::new(),
        takes_arguments: true,
        requires_argument: true,
        requires_required_argument: false,
        composer_wants_trailing_space: true,
        palette_runs_directly: false,
        show_in_empty_discovery: true,
        discovery: codewhale_command_contract::CommandDiscovery::Primary,
        unlisted: false,
    }
}

fn user(name: &str) -> UserCommand {
    UserCommand {
        name: name.to_string(),
        aliases: Vec::new(),
        description: Some("Pick the fast route.".to_string()),
        usage: None,
        takes_arguments: false,
        hidden: false,
        extension: false,
        builtin_bindings: vec![name.to_string()],
    }
}

struct Catalog(PathBuf);
impl CommandCatalog for Catalog {
    fn answers_to(&self, name: &str) -> bool {
        matches!(name, "model" | "profile")
    }
    fn builtins(&self) -> Vec<BuiltinCommand> {
        let mut profile = builtin("profile", Vec::new());
        profile.requires_required_argument = true;
        vec![builtin("model", Vec::new()), profile]
    }
    fn user_commands(&self, workspace: &Path) -> Vec<UserCommand> {
        if workspace == self.0 {
            vec![user("model")]
        } else {
            Vec::new()
        }
    }
}

fn entry<'a>(commands: &'a [CommandCatalogEntry], name: &str) -> &'a CommandCatalogEntry {
    commands
        .iter()
        .find(|command| command.name == name)
        .unwrap_or_else(|| panic!("catalog must contain {name}"))
}

#[test]
fn command_catalog_marks_user_shadowing_of_builtin_names_and_aliases() {
    let builtins = [
        builtin("model", vec![]),
        builtin("subagents", vec!["agents".to_string()]),
    ];
    let users = [user("model"), user("agents")];
    let commands = command_catalog(&builtins, &users);

    let model = entry(&commands, "model");
    assert_eq!(model.shadowed_by.as_deref(), Some("model"));

    // The shadowing user command is served as a prompt-bound row.
    let user_model = commands
        .iter()
        .find(|command| command.name == "model" && command.kind == "user")
        .expect("user command row");
    assert_eq!(user_model.binding, "prompt");

    // A user command colliding with a builtin's alias shadows that spelling:
    // `agents` is a `subagents` alias, so the builtin reports it while keeping
    // its canonical name.
    let subagents = entry(&commands, "subagents");
    assert_eq!(subagents.kind, "builtin");
    assert_eq!(subagents.shadowed_by, None);
    assert!(
        subagents.shadowed_aliases.iter().any(|a| a == "agents"),
        "shadowed_aliases must report the taken spelling: {:?}",
        subagents.shadowed_aliases
    );
}

#[tokio::test]
async fn get_v1_commands_serves_the_catalog_over_http() -> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("commands-route");
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let _catalog =
        crate::command_catalog::CatalogGuard::install(Arc::new(Catalog(workspace.clone())));

    let Some((addr, runtime_threads, handle)) = spawn_test_server_with_root_token_mobile_workspace(
        root,
        sessions_dir,
        None,
        false,
        workspace,
    )
    .await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();

    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/commands"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let commands = body["commands"].as_array().expect("commands array");
    assert!(
        commands
            .iter()
            .any(|command| command["kind"] == "builtin" && command["binding"] == "host"),
        "the route must serve builtins: {commands:?}"
    );

    // The workspace user command named `model` shadows the builtin: the
    // builtin reports the shadow and the winning definition is served.
    let builtin_model = commands
        .iter()
        .find(|command| command["name"] == "model" && command["kind"] == "builtin")
        .expect("builtin model row");
    assert_eq!(builtin_model["shadowed_by"], "model");
    let user_model = commands
        .iter()
        .find(|command| command["name"] == "model" && command["kind"] == "user")
        .expect("user model row");
    assert_eq!(user_model["binding"], "prompt");
    // A template without `$ARGUMENTS` runs bare from the palette.
    assert_eq!(user_model["requires_required_argument"], false);
    assert_eq!(user_model["palette_runs_directly"], true);
    assert_eq!(user_model["show_in_empty_discovery"], true);

    let profile = commands
        .iter()
        .find(|command| command["name"] == "profile" && command["kind"] == "builtin")
        .expect("builtin profile row");
    assert_eq!(profile["requires_required_argument"], true);
    assert_eq!(profile["palette_runs_directly"], false);

    assert_eq!(
        client
            .get(format!(
                "http://{addr}/v1/commands?thread_id=missing-thread"
            ))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    let thread = runtime_threads
        .create_thread(CreateThreadRequest::default())
        .await?;
    let operation_url = format!(
        "http://{addr}/v1/threads/{}/commands/cancel-before-admission",
        thread.id
    );
    assert_eq!(
        client.get(&operation_url).send().await?.status(),
        StatusCode::NOT_FOUND
    );
    let cancelled: Value = client
        .delete(&operation_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(cancelled["cancelled"], true);
    let receipt: Value = client
        .get(&operation_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(receipt["status"], "cancelled");
    assert_eq!(receipt["operation_key"], "cancel-before-admission");
    let response = client
        .post(format!("http://{addr}/v1/threads/{}/commands", thread.id))
        .json(&json!({
            "name": "native", "args": "", "registration_id": "a".repeat(64),
            "operation_key": "cancel-before-admission", "expected_workspace": thread.workspace,
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let refusal: Value = response.json().await?;
    assert_eq!(refusal["error"]["code"], "command_cancelled");
    let cancelled_again: Value = client
        .delete(&operation_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(cancelled_again["cancelled"], true);

    handle.abort();
    let Some(node) = crate::extension_host::tests::node_for_tests("runtime_command_catalog") else {
        return Ok(());
    };
    let _policy = crate::plugins::activation::TestPolicyGuard::extension_host(true);
    let fixture = crate::extension_host::tests::FixturePlugins::new(&[]).await;
    let source = temp.path().join("native-command-plugin");
    fs::create_dir_all(&source)?;
    fs::write(
        source.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"native-command-fixture","version":"0.1.0","description":"Runtime command fixture","license":"MIT","extensions":{"net.codewhale":{"native":{"path":"index.mjs"}}}}"#,
    )?;
    fs::write(
        source.join("index.mjs"),
        r#"
export const name = 'native-command-fixture'
export const inject = ['commands']
export function apply(ctx) {
  let shows = 0
  ctx.commands.register({ name: 'native-show', description: 'Show a result', handler: () => String(++shows) })
  ctx.commands.register({ name: 'native-submit', description: 'Submit a turn', handler: () => ({ kind: 'submit', prompt: 'Command-generated prompt', text: 'Submitted by plugin' }) })
  ctx.commands.register({ name: 'native-fail', description: 'Fail visibly', handler: () => ({ kind: 'error', text: 'Expected fixture failure' }) })
  ctx.commands.register({ name: 'native-slow', description: 'Wait for cancellation', handler: async () => new Promise(resolve => setTimeout(() => resolve('late result'), 10000)) })
}
"#,
    )?;
    crate::plugins::install::install(
        crate::plugins::install::PluginInstallSource::LocalPath(source),
        &fixture.config.user_plugins_dir,
        crate::plugins::install::DEFAULT_MAX_SIZE_BYTES,
        &codewhale_net::network_policy::NetworkPolicy::default(),
        false,
        &|_| None,
    )
    .await?;
    let mut plugins = crate::plugins::discovery::discover_with_config(&fixture.config);
    plugins
        .trust("native-command-fixture")
        .map_err(anyhow::Error::msg)?;
    plugins
        .enable("native-command-fixture")
        .map_err(anyhow::Error::msg)?;
    let extension_manager = fixture.manager(node);
    let _extension_manager =
        crate::extension_host::TestManagerGuard::install(extension_manager.clone());
    let mut config = Config::default().with_legacy_root(
        Some("command-fixture-key".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    config.set_feature("extension_host", true)?;
    let (addr, runtime_threads, handle) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            temp.path().join("native-runtime"),
            temp.path().join("native-sessions"),
            None,
            false,
            fixture.workspace().to_path_buf(),
            TestServerOverrides {
                config: Some(config),
                plugin_discovery: Some(
                    crate::plugins::PluginDiscoveryContext::from_config_and_environment(
                        &fixture.config,
                        crate::plugins::HostEnvironment::from_entries(Vec::new()),
                    ),
                ),
                ..TestServerOverrides::default()
            },
        )
        .await?
        .context("native command acceptance requires a loopback listener")?;
    let model = Arc::new(crate::llm_client::mock::MockLlmClient::new(vec![
        crate::llm_client::mock::canned::simple_text_turn("Plugin turn completed"),
    ]));
    runtime_threads.set_test_model_client(model.clone());
    let thread = runtime_threads
        .create_thread(CreateThreadRequest {
            model: Some("deepseek-v4-pro".into()),
            ..Default::default()
        })
        .await?;
    let catalog: Value = client
        .get(format!("http://{addr}/v1/commands?thread_id={}", thread.id))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let rows = catalog["commands"].as_array().context("command rows")?;
    let show = rows
        .iter()
        .find(|row| row["name"] == "native-show")
        .context("live native command")?;
    let submit = rows
        .iter()
        .find(|row| row["name"] == "native-submit")
        .context("live submit command")?;
    assert_eq!(show["binding"], "extension");
    assert_eq!(show["origin"], "extension:native-command-fixture");
    let command_url = format!("http://{addr}/v1/threads/{}/commands", thread.id);
    let show_request = json!({ "name":"native-show", "args":"", "registration_id":show["registration_id"], "operation_key":"show-once", "expected_workspace":thread.workspace });
    let shown: Value = client
        .post(&command_url)
        .json(&show_request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(shown["status"], "completed");
    assert_eq!(shown["text"], "1");
    assert_eq!(shown["idempotent_replay"], false);
    assert!(
        shown["extension"]["content_hash"]
            .as_str()
            .is_some_and(|value| value.len() == 64)
    );
    assert!(shown["extension"]["entry"]["sha256"].is_string());
    assert!(shown["extension"]["generation"].is_number());
    let replay: Value = client
        .post(&command_url)
        .json(&show_request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(replay["text"], "1");
    assert_eq!(replay["idempotent_replay"], true);
    let lookup: Value = client
        .get(format!("{command_url}/show-once"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(lookup, replay);
    assert!(model.captured_requests().is_empty());
    let failure = rows
        .iter()
        .find(|row| row["name"] == "native-fail")
        .context("live failure command")?;
    let failed: Value = client.post(&command_url).json(&json!({ "name":"native-fail", "args":"", "registration_id":failure["registration_id"], "operation_key":"failure-once", "expected_workspace":thread.workspace })).send().await?.error_for_status()?.json().await?;
    assert_eq!(failed["status"], "failed");
    let slow = rows
        .iter()
        .find(|row| row["name"] == "native-slow")
        .context("live slow command")?;
    let slow_request = json!({ "name":"native-slow", "args":"", "registration_id":slow["registration_id"], "operation_key":"cancel-running", "expected_workspace":thread.workspace });
    let slow_client = client.clone();
    let slow_url = command_url.clone();
    let waiting =
        tokio::spawn(async move { slow_client.post(&slow_url).json(&slow_request).send().await });
    tokio::time::timeout(ci_scaled(Duration::from_secs(10)), async {
        loop {
            let response = client
                .get(format!("{command_url}/cancel-running"))
                .send()
                .await?;
            if response.status().is_success() {
                let pending: Value = response.json().await?;
                assert_eq!(pending["status"], "pending");
                break;
            }
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    client
        .delete(format!("{command_url}/cancel-running"))
        .send()
        .await?
        .error_for_status()?;
    let cancelled: Value = waiting.await??.error_for_status()?.json().await?;
    assert_eq!(cancelled["status"], "cancelled");
    let snapshot: Value = client
        .get(format!("http://{addr}/v1/threads/{}", thread.id))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let receipts = snapshot["command_receipts"]
        .as_array()
        .context("snapshot command receipts")?;
    assert_eq!(receipts.len(), 3);
    assert_eq!(receipts[0]["text"], "1");
    assert_eq!(receipts[1]["status"], "failed");
    assert_eq!(receipts[2]["status"], "cancelled");
    assert!(receipts.iter().all(|receipt| {
        receipt["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("command_"))
            && receipt["created_at"].is_string()
    }));
    let submit_request = json!({ "name":"native-submit", "args":"", "registration_id":submit["registration_id"], "operation_key":"submit-once", "expected_workspace":thread.workspace });
    let submitted: Value = client
        .post(&command_url)
        .json(&submit_request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(submitted["status"], "completed", "{submitted}");
    let turn_id = submitted["turn"]["id"]
        .as_str()
        .context("Engine-admitted turn")?;
    assert_eq!(
        wait_for_terminal_turn_status(&client, addr, &thread.id, turn_id, Duration::from_secs(20))
            .await?,
        "completed"
    );
    let replayed_submit: Value = client
        .post(&command_url)
        .json(&submit_request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(replayed_submit["turn"]["id"], turn_id);
    assert_eq!(replayed_submit["idempotent_replay"], true);
    assert_eq!(model.captured_requests().len(), 1);
    let detail = runtime_threads.get_thread_detail(&thread.id).await?;
    assert_eq!(
        serde_json::to_value(&detail.command_receipts)?,
        snapshot["command_receipts"]
    );
    let user_item = detail
        .items
        .iter()
        .find(|item| item.kind == TurnItemKind::UserMessage)
        .context("command input item")?;
    assert_eq!(
        user_item.metadata.as_ref().context("command provenance")?["input_provenance"],
        "extension_command"
    );
    assert_eq!(
        user_item.detail.as_deref(),
        Some("Command-generated prompt")
    );
    let owner: Value = serde_json::from_slice(&fs::read(
        runtime_threads
            .session_store_binding()
            .data_dir
            .join("owner.json"),
    )?)?;
    let owner_id = owner["owner_id"].as_str().context("Runtime store owner")?;
    for (key, original_request, original_result, recover_turn) in [
        ("orphan-show", &show_request, &shown, false),
        ("orphan-submit", &submit_request, &submitted, true),
    ] {
        let mut request = original_request.clone();
        request["operation_key"] = json!(key);
        let fingerprint = codewhale_runtime::hashing::sha256_hex(format!(
            "runtime-turn-operation{sep}{owner_id}{sep}{}{sep}{key}",
            thread.id,
            sep = char::from(31)
        ));
        let request_fingerprint = codewhale_runtime::hashing::sha256_hex(
            codewhale_runtime::hashing::canonical_json(&json!({
                "name":request["name"], "args":request["args"], "registration_id":request["registration_id"], "expected_workspace":request["expected_workspace"],
            })),
        );
        runtime_threads.emit_event_for_test(&thread.id, None, "extension_command_started", json!({
            "operation_fingerprint":fingerprint, "request_fingerprint":request_fingerprint,
            "name":request["name"], "args":request["args"], "extension":original_result["extension"],
        })).await?;
        let admitted = if recover_turn {
            model.push_turn(crate::llm_client::mock::canned::simple_text_turn(
                "Recovered admitted turn",
            ));
            let turn = runtime_threads
                .start_turn(
                    &thread.id,
                    StartTurnRequest {
                        prompt: "Already-admitted command turn".into(),
                        operation_key: Some(format!("extension-command-{fingerprint}")),
                        ..Default::default()
                    },
                )
                .await?;
            wait_for_terminal_turn_status(
                &client,
                addr,
                &thread.id,
                &turn.id,
                Duration::from_secs(20),
            )
            .await?;
            let store = crate::runtime_threads::RuntimeThreadStore::open(
                runtime_threads.session_store_binding().data_dir,
            )?;
            let mut input =
                store.load_item(turn.item_ids.first().context("orphan command input")?)?;
            input.metadata = Some(json!({
                "input_provenance": "extension_command", "command_operation": fingerprint,
                "extension": original_result["extension"],
            }));
            store.save_item(&input)?;
            client
                .delete(format!("{command_url}/{key}"))
                .send()
                .await?
                .error_for_status()?;
            Some(turn)
        } else {
            None
        };
        let recovered: Value = client
            .get(format!("{command_url}/{key}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(recovered["status"], "failed");
        assert!(
            recovered["text"]
                .as_str()
                .is_some_and(|text| text.contains("uncertain") && text.contains("not rerun"))
        );
        assert_eq!(recovered["extension"], original_result["extension"]);
        if let Some(turn) = admitted {
            assert_eq!(recovered["turn"]["id"], turn.id);
        }
        let replay: Value = client
            .post(&command_url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(replay, recovered);
        let repeated: Value = client
            .get(format!("{command_url}/{key}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(repeated, recovered);
    }
    for (key, owned) in [
        ("submit-acknowledgement-uncertain", true),
        ("submit-unrelated-key-conflict", false),
    ] {
        let mut uncertain_request = submit_request.clone();
        uncertain_request["operation_key"] = json!(key);
        let uncertain_fingerprint = codewhale_runtime::hashing::sha256_hex(format!(
            "runtime-turn-operation{sep}{owner_id}{sep}{}{sep}{key}",
            thread.id,
            sep = char::from(31)
        ));
        model.push_turn(crate::llm_client::mock::canned::simple_text_turn(
            "Existing operation result",
        ));
        let accepted = runtime_threads
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "Existing operation input".into(),
                    operation_key: Some(format!("extension-command-{uncertain_fingerprint}")),
                    ..Default::default()
                },
            )
            .await?;
        wait_for_terminal_turn_status(
            &client,
            addr,
            &thread.id,
            &accepted.id,
            Duration::from_secs(20),
        )
        .await?;
        if owned {
            let store = crate::runtime_threads::RuntimeThreadStore::open(
                runtime_threads.session_store_binding().data_dir,
            )?;
            let mut input =
                store.load_item(accepted.item_ids.first().context("owned command input")?)?;
            input.metadata = Some(json!({
                "input_provenance": "extension_command", "command_operation": uncertain_fingerprint,
                "extension": submitted["extension"],
            }));
            store.save_item(&input)?;
        }
        let uncertain: Value = client
            .post(&command_url)
            .json(&uncertain_request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(uncertain["status"], "failed");
        if owned {
            assert_eq!(uncertain["turn"]["id"], accepted.id);
            assert_eq!(uncertain["thread"]["id"], thread.id);
            assert!(uncertain["text"].as_str().is_some_and(|text| {
                text.contains("uncertain")
                    && text.contains("recovered")
                    && text.contains("not rerun")
            }));
        } else {
            assert!(uncertain["turn"].is_null());
            assert!(uncertain["thread"].is_null());
            assert!(uncertain["text"].as_str().is_some_and(|text| {
                text.contains("no admitted turn was recovered")
                    && text.contains("different turn request")
            }));
        }
        let uncertain_replay: Value = client
            .post(&command_url)
            .json(&uncertain_request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(uncertain_replay["turn"], uncertain["turn"]);
        assert_eq!(uncertain_replay["text"], uncertain["text"]);
        assert_eq!(uncertain_replay["idempotent_replay"], true);
    }
    assert_eq!(model.captured_requests().len(), 4);
    let mut fresh_show = show_request.clone();
    fresh_show["operation_key"] = json!("after-orphans");
    let fresh: Value = client
        .post(&command_url)
        .json(&fresh_show)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(fresh["text"], "2");
    assert!(
        runtime_threads
            .get_thread_detail(&thread.id)
            .await?
            .command_receipts
            .iter()
            .any(|receipt| receipt.text.contains("uncertain"))
    );
    plugins
        .disable("native-command-fixture")
        .map_err(anyhow::Error::msg)?;
    crate::extension_host::plugins_changed(Arc::new(plugins));
    extension_manager
        .reconcile()
        .await
        .map_err(anyhow::Error::msg)?;
    let mut plugins = crate::plugins::discovery::discover_with_config(&fixture.config);
    plugins
        .enable("native-command-fixture")
        .map_err(anyhow::Error::msg)?;
    crate::extension_host::plugins_changed(Arc::new(plugins));
    extension_manager
        .reconcile()
        .await
        .map_err(anyhow::Error::msg)?;
    let mut stale_request = show_request.clone();
    stale_request["operation_key"] = json!("stale-after-reload");
    let stale = client
        .post(&command_url)
        .json(&stale_request)
        .send()
        .await?;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale: Value = stale.json().await?;
    assert_eq!(stale["error"]["code"], "command_not_started");
    let old_replay: Value = client
        .post(&command_url)
        .json(&show_request)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(old_replay["extension"], shown["extension"]);
    assert_eq!(old_replay["text"], "1");
    runtime_threads.shutdown_and_wait().await?;
    extension_manager.shutdown().await;
    handle.abort();
    Ok(())
}

/// B4: `GET /v1/hooks` serves the hook set Runtime threads run, from the
/// engine's own loader, with credential-shaped values masked.
#[tokio::test]
async fn get_v1_hooks_serves_the_runtime_hook_set() -> Result<()> {
    let _env = lock_test_env();
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("hooks-route");
    let sessions_dir = root.join("sessions");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let config = {
        let __cw_config_init_0_0 = Some(codewhale_config::hooks::HooksConfig {
        hooks: vec![
            codewhale_config::hooks::Hook::new(
                codewhale_config::hooks::HookEvent::ToolCallAfter,
                "curl -H 'Authorization: Bearer sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789' https://example.invalid",
            )
            .with_name("notify"),
        ],
        enabled: true,
        ..codewhale_config::hooks::HooksConfig::default()
    });
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.hooks = __cw_config_init_0_0;
        __cw_config_init_0_value
    };

    let Some((addr, _runtime_threads, handle)) =
        spawn_test_server_with_root_token_mobile_workspace_and_overrides(
            root,
            sessions_dir,
            None,
            false,
            workspace,
            TestServerOverrides {
                config: Some(config),
                ..TestServerOverrides::default()
            },
        )
        .await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let body: serde_json::Value = client
        .get(format!("http://{addr}/v1/hooks"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(body["enabled"], true);
    let hooks = body["hooks"].as_array().expect("hooks array");
    assert_eq!(hooks.len(), 1, "{body}");
    assert_eq!(hooks[0]["name"], "notify");
    assert_eq!(hooks[0]["event"], "tool_call_after");
    assert_eq!(hooks[0]["source"], "global");
    let command = hooks[0]["command"].as_str().expect("command");
    assert!(command.starts_with("curl"), "{command}");
    assert!(!command.contains("sk-ant-api03-AbCd"), "{command}");

    let missing = client
        .get(format!("http://{addr}/v1/hooks?thread_id=thr_missing"))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    handle.abort();
    Ok(())
}

#[test]
fn hook_listing_masks_url_paths_and_userinfo() {
    assert_eq!(
        redact_hook_command_for_listing(
            "curl -X POST https://hooks.slack.com/services/T000/B000/XXXXsecret -d @-"
        ),
        "curl -X POST https://hooks.slack.com/[redacted] -d @-"
    );
    assert_eq!(
        redact_hook_command_for_listing("notify 'https://user:pw@example.test/hook?k=v'"),
        "notify 'https://example.test/[redacted]'"
    );
    assert_eq!(
        redact_hook_command_for_listing("ping https://example.test"),
        "ping https://example.test"
    );
    assert_eq!(
        redact_hook_command_for_listing("./scripts/lint.sh --fix"),
        "./scripts/lint.sh --fix"
    );
}
