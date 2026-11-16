use super::*;

const SEATS: [&str; 6] = [
    "customize.plugins.detail",
    "conversation.composer.dock",
    "conversation.chat.node",
    "workpane.panel",
    "sidebar.section",
    "command.row",
];

const CONSUMER_BRIDGE_KEYS: [&str; 7] = [
    "kind",
    "dsh_version",
    "cordis_version",
    "compatibility_digest",
    "declared_range",
    "matched_version",
    "admission",
];

const CONSUMER_REFUSALS: [(u16, &str); 6] = [
    (404, "plugin_ui_not_declared"),
    (404, "plugin_ui_seat_not_admitted"),
    (400, "plugin_ui_unsupported"),
    (422, "plugin_ui_seat_unsupported"),
    (400, "plugin_ui_seat_unknown"),
    (404, "plugin_ui_file_not_reviewed"),
];

const DSH_CLIENT: &str = "const react = require(\"react\");\n\
const util = require(\"./util\");\n\
const keys = [\"sidebar.right.pane.tab\", \"settings.section\", \"conversation.session.header.utilities\"];\n\
exports.apply = function apply(ctx) { return [react, util.v, keys, ctx]; };\n";

fn write_dsh_package(dir: &Path, name: &str, client: &str) -> Result<PathBuf> {
    let package = dir.join(name);
    fs::create_dir_all(package.join("lib"))?;
    fs::write(
        package.join("package.json"),
        serde_json::json!({
            "name": format!("@demo/{name}"),
            "version": "1.0.0",
            "exports": { "./client": { "default": "./lib/client.js" } },
            "dsh": {
                "bundle": { "patch": "./cordis.patch.yml" },
                "client": { "platform": "web" }
            }
        })
        .to_string(),
    )?;
    fs::write(
        package.join("cordis.patch.yml"),
        "- insert:\n  - id: skills\n    name: \"@deepseek-ai/dsh-skill-filesystem\"\n    config:\n      customSkillDirs:\n        - ./skills\n",
    )?;
    fs::create_dir_all(package.join("skills/greet"))?;
    fs::write(
        package.join("skills/greet/SKILL.md"),
        "---\nname: greet\ndescription: Greets a person\n---\nSay hi.\n",
    )?;
    fs::write(package.join("lib/client.js"), client)?;
    fs::write(package.join("lib/util.js"), "exports.v = 1;\n")?;
    Ok(package)
}

fn write_native_ui_bundle(dir: &Path, name: &str) -> Result<PathBuf> {
    let bundle = write_plugin_source_bundle(dir, name)?;
    fs::write(
        bundle.join("package.json"),
        serde_json::json!({
            "name": name,
            "version": "1.0.0",
            "exports": { "./client": "./client.js" },
            "dsh": { "client": { "platform": "web" } }
        })
        .to_string(),
    )?;
    fs::write(bundle.join("client.js"), "window.__native = true;\n")?;
    Ok(bundle)
}

pub(super) async fn install_active(
    client: &reqwest::Client,
    base: &str,
    source: String,
) -> Result<serde_json::Value> {
    let install: serde_json::Value = client
        .post(format!("{base}/install"))
        .json(&serde_json::json!({ "source": source }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let name = install["name"]
        .as_str()
        .expect("installed name")
        .to_string();
    let detail: serde_json::Value = client
        .get(format!("{base}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = detail["review"]["token"].as_str().unwrap().to_string();
    client
        .post(format!("{base}/{name}/trust"))
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await?
        .error_for_status()?;
    let enabled: serde_json::Value = client
        .post(format!("{base}/{name}/enable"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(enabled["state"], "active", "{enabled}");
    Ok(serde_json::json!({ "name": name, "path": detail["path"] }))
}

async fn ui(
    client: &reqwest::Client,
    base: &str,
    name: &str,
    query: &str,
) -> Result<(u16, serde_json::Value)> {
    let response = client
        .get(format!("{base}/{name}/ui?{query}"))
        .send()
        .await?;
    let status = response.status().as_u16();
    Ok((status, response.json().await?))
}

async fn mutate(client: &reqwest::Client, url: String) -> Result<(StatusCode, String)> {
    let response = client.post(url).send().await?;
    Ok((response.status(), response.text().await?))
}

fn refusal(status: u16, body: &serde_json::Value) -> (u16, String) {
    (
        status,
        body["error"]["code"].as_str().unwrap_or("").to_string(),
    )
}

fn assert_consumer_contract(name: &str, minted: &serde_json::Value, seat: &str, key: &str) {
    assert_eq!(minted["slot"], seat);
    assert_eq!(minted["dsh_slot"], key);
    assert!(SEATS.contains(&minted["slot"].as_str().unwrap()));
    let key = minted["dsh_slot"].as_str().unwrap();
    assert!(key.len() <= 128 && key.chars().next().unwrap().is_ascii_alphanumeric());
    assert!(
        key.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    );
    let bridge = minted["bridge"].as_object().expect("bridge object");
    assert_eq!(bridge["kind"], "dsh-client");
    for (field, value) in bridge {
        assert!(CONSUMER_BRIDGE_KEYS.contains(&field.as_str()), "{field}");
        assert!(
            value
                .as_str()
                .is_some_and(|v| !v.is_empty() && v.len() <= 128)
        );
    }
    assert!(bridge.contains_key("dsh_version"));
    let base = minted["base"].as_str().unwrap();
    assert!(base.starts_with("http://127.0.0.1:"));
    assert!(base.ends_with(&format!(
        "/v1/apps/plugins/{}/ui/",
        urlencoding::encode(name)
    )));
    let entry = minted["entry"].as_str().unwrap();
    assert!(!entry.is_empty() && entry.len() <= 512 && !entry.contains(".."));
    assert!(
        minted["revision"]
            .as_str()
            .is_some_and(|r| r.starts_with('"'))
    );
}

fn bootstrap_loads_in_node(source: &str) -> Option<String> {
    let dir = tempfile::tempdir().ok()?;
    let script = dir.path().join("run.js");
    let harness = format!(
        "var captured;\nvar window = {{ __ModuleLoader__: {{ load: function (r) {{ captured = r; }} }} }};\n{source}\nvar mod = captured.factory(function (id) {{ return {{ id: id }}; }});\nprocess.stdout.write(JSON.stringify({{ id: captured.id, apply: typeof mod.apply, out: mod.apply('ctx') }}));\n"
    );
    fs::write(&script, harness).ok()?;
    let output = std::process::Command::new("node")
        .arg(&script)
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "the served bootstrap failed under node: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[tokio::test]
async fn plugin_ui_dsh_client_serves_admitted_seats_and_denies_and_revokes() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    let package = write_dsh_package(tmp.path(), "panel", DSH_CLIENT)?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");
    let installed = install_active(&client, &base, format!("dsh:{}", package.display())).await?;
    let name = installed["name"].as_str().unwrap().to_string();

    let (status, panel) = ui(&client, &base, &name, "slot=workpane.panel").await?;
    assert_eq!(status, 200, "{panel}");
    assert_consumer_contract(&name, &panel, "workpane.panel", "sidebar.right.pane.tab");
    assert_eq!(panel["entry"], "__codewhale_dsh_bootstrap.js");
    let (status, detail) = ui(&client, &base, &name, "slot=customize.plugins.detail").await?;
    assert_eq!(status, 200, "{detail}");
    assert_consumer_contract(
        &name,
        &detail,
        "customize.plugins.detail",
        "settings.section",
    );
    let (status, explicit) = ui(
        &client,
        &base,
        &name,
        "slot=workpane.panel&dsh_slot=sidebar.right.pane.tab",
    )
    .await?;
    assert_eq!(status, 200, "{explicit}");

    for (query, expected) in [
        ("slot=sidebar.section", (404, "plugin_ui_seat_not_admitted")),
        ("slot=command.row", (404, "plugin_ui_seat_not_admitted")),
        (
            "slot=workpane.panel&dsh_slot=main.page-menu",
            (404, "plugin_ui_seat_not_admitted"),
        ),
        (
            "slot=workpane.panel&dsh_slot=settings.section",
            (404, "plugin_ui_seat_not_admitted"),
        ),
        (
            "slot=workpane.panel&dsh_slot=conversation.session.header.utilities",
            (422, "plugin_ui_seat_unsupported"),
        ),
        ("slot=chat.node", (400, "plugin_ui_seat_unknown")),
        ("dsh_slot=settings.section", (400, "plugin_ui_seat_unknown")),
        (
            "slot=workpane.panel&dsh_slot=..%2Fx",
            (400, "plugin_ui_dsh_slot_invalid"),
        ),
    ] {
        let (status, body) = ui(&client, &base, &name, query).await?;
        let (status, code) = refusal(status, &body);
        assert_eq!((status, code.as_str()), expected, "{query}: {body}");
        assert!(
            CONSUMER_REFUSALS.contains(&(status, code.as_str()))
                || code == "plugin_ui_dsh_slot_invalid",
            "{query}"
        );
    }

    let revision = panel["revision"].as_str().unwrap().to_string();
    let asset_url = format!(
        "{}{}",
        panel["base"].as_str().unwrap(),
        panel["entry"].as_str().unwrap()
    );
    let served = client
        .get(&asset_url)
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(served.status(), StatusCode::OK);
    assert!(
        served
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/javascript"))
    );
    let bootstrap = served.text().await?;
    assert!(bootstrap.contains("\"source/lib/util.js\""));
    assert!(!bootstrap.contains("Bearer"));
    match bootstrap_loads_in_node(&bootstrap) {
        Some(out) => {
            let out: serde_json::Value = serde_json::from_str(&out)?;
            assert_eq!(out["apply"], "function");
            assert_eq!(out["out"][0]["id"], "react");
            assert_eq!(out["out"][1], 1);
        }
        None => eprintln!("node is unavailable; the bootstrap execution check was skipped"),
    }

    let asset_base = panel["base"].as_str().unwrap().to_string();
    let missing = client.get(&asset_url).send().await?;
    assert_eq!(missing.status(), StatusCode::PRECONDITION_REQUIRED);
    let wrong = client
        .get(&asset_url)
        .header("If-Match", "\"not-a-revision\"")
        .send()
        .await?;
    assert_eq!(wrong.status(), StatusCode::PRECONDITION_FAILED);
    for outside in [
        "package.json",
        "source/package.json",
        "source/lib/index.cjs",
        "source/lib/util.js",
    ] {
        let response = client
            .get(format!("{asset_base}{outside}"))
            .header("If-Match", &revision)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{outside}");
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["error"]["code"], "plugin_ui_file_not_reviewed");
    }
    let traversal = client
        .get(format!("{asset_base}%2e%2e/%2e%2e/etc/passwd"))
        .header("If-Match", &revision)
        .send()
        .await?;
    assert!(traversal.status().is_client_error());

    let route = format!("{base}/{name}/routes/api/state");
    let routed = client
        .post(&route)
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(routed.status(), StatusCode::NOT_IMPLEMENTED);
    let body: serde_json::Value = routed.json().await?;
    assert_eq!(body["error"]["code"], "plugin_route_not_served");
    assert_eq!(
        client.post(&route).send().await?.status(),
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        client
            .post(&route)
            .header("If-Match", "\"not-a-revision\"")
            .send()
            .await?
            .status(),
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        client
            .post(format!("{base}/other/routes/api/state"))
            .header("If-Match", &revision)
            .send()
            .await?
            .status(),
        StatusCode::PRECONDITION_FAILED
    );
    assert!(
        client
            .post(format!("{base}/{name}/routes/%2e%2e/x"))
            .header("If-Match", &revision)
            .send()
            .await?
            .status()
            .is_client_error()
    );

    let (status, body) = mutate(&client, format!("{base}/{name}/disable")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    for url in [&asset_url, &route] {
        let response = client.get(url).header("If-Match", &revision).send().await?;
        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED, "{url}");
    }
    let (status, body) = mutate(&client, format!("{base}/{name}/enable")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stale = client
        .get(&asset_url)
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

    let (status, again) = ui(&client, &base, &name, "slot=workpane.panel").await?;
    assert_eq!(status, 200, "{again}");
    let again_revision = again["revision"].as_str().unwrap().to_string();
    assert_ne!(again_revision, revision);
    let staged = PathBuf::from(installed["path"].as_str().unwrap()).join("source/lib/util.js");
    let live = client
        .get(&asset_url)
        .header("If-Match", &again_revision)
        .send()
        .await?;
    assert_eq!(live.status(), StatusCode::OK);
    fs::write(&staged, "exports.v = 2;\n")?;
    let tampered = client
        .get(&asset_url)
        .header("If-Match", &again_revision)
        .send()
        .await?;
    assert_eq!(tampered.status(), StatusCode::PRECONDITION_FAILED);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn plugin_ui_dsh_client_with_unprovided_modules_or_ambient_dependencies_is_unsupported()
-> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    let host_module = write_dsh_package(
        tmp.path(),
        "hostmod",
        "require(\"@deepseek-ai/dsh-client-ui-primitives\");\nconst k = \"sidebar.right.pane.tab\";\n",
    )?;
    let esm = write_dsh_package(
        tmp.path(),
        "esm",
        "const k = \"sidebar.right.pane.tab\";\nexport const apply = () => k;\n",
    )?;
    let ambient = write_dsh_package(
        tmp.path(),
        "ambient",
        "require(\"left-pad\");\nconst k = \"sidebar.right.pane.tab\";\n",
    )?;

    let Some((addr, handle)) = spawn_plugin_api_server(root, workspace).await? else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");
    for package in [host_module, esm, ambient] {
        let installed =
            install_active(&client, &base, format!("dsh:{}", package.display())).await?;
        let name = installed["name"].as_str().unwrap();
        let (status, body) = ui(&client, &base, name, "slot=workpane.panel").await?;
        assert_eq!(
            refusal(status, &body),
            (400, "plugin_ui_unsupported".to_string()),
            "{name}: {body}"
        );
    }
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn plugin_ui_native_client_serves_only_the_detail_seat_and_is_revoked() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    fs::create_dir_all(&root)?;
    let bundle = write_native_ui_bundle(tmp.path(), "nativeui")?;

    let Some((addr, handle)) = spawn_plugin_api_server(root.clone(), workspace.clone()).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");
    let installed = install_active(&client, &base, format!("path:{}", bundle.display())).await?;
    let name = installed["name"].as_str().unwrap().to_string();
    let recorded =
        isolated_plugin_discovery(&root, &workspace).inspect_registry_for_workspace(&workspace);
    let by_app = codewhale_plugins::host_services::PluginApprover::person(
        codewhale_plugins::host_services::PersonVia::App,
    );
    let plugin = recorded.get(&name).unwrap();
    assert_eq!(plugin.approved_by.as_ref(), Some(&by_app));
    assert_eq!(plugin.enabled_by.as_ref(), Some(&by_app));

    let (status, minted) = ui(&client, &base, &name, "slot=customize.plugins.detail").await?;
    assert_eq!(status, 200, "{minted}");
    assert!(minted.get("dsh_slot").is_none() && minted.get("bridge").is_none());
    assert_eq!(minted["entry"], "client.js");
    for query in [
        "slot=workpane.panel",
        "slot=sidebar.section",
        "slot=customize.plugins.detail&dsh_slot=settings.section",
    ] {
        let (status, body) = ui(&client, &base, &name, query).await?;
        assert_eq!(
            refusal(status, &body),
            (404, "plugin_ui_seat_not_admitted".to_string()),
            "{query}"
        );
    }

    let revision = minted["revision"].as_str().unwrap().to_string();
    let asset = format!("{}client.js", minted["base"].as_str().unwrap());
    let served = client
        .get(&asset)
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(served.status(), StatusCode::OK);
    let unlisted = client
        .get(format!("{}nothing.js", minted["base"].as_str().unwrap()))
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(unlisted.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = unlisted.json().await?;
    assert_eq!(body["error"]["code"], "plugin_ui_file_not_reviewed");
    let routed = client
        .post(format!("{base}/{name}/routes/x"))
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(routed.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = routed.json().await?;
    assert_eq!(body["error"]["code"], "plugin_route_not_admitted");

    let (status, body) = mutate(&client, format!("{base}/{name}/revoke")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = client
        .get(&asset)
        .header("If-Match", &revision)
        .send()
        .await?;
    assert_eq!(after.status(), StatusCode::PRECONDITION_FAILED);

    handle.abort();
    Ok(())
}
