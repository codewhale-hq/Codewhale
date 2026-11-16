use super::*;
use codewhale_plugins::host_services::{
    AgentApproval, AgentPosture, PluginApprover, ReviewApproval,
};

fn write_bundle_with_host_services(
    dir: &Path,
    name: &str,
    host_services: &[&str],
) -> Result<PathBuf> {
    let bundle = write_plugin_source_bundle(dir, name)?;
    if !host_services.is_empty() {
        let joined = host_services
            .iter()
            .map(|service| format!("\"{service}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let existing = fs::read_to_string(bundle.join("plugin.toml"))?;
        fs::write(
            bundle.join("plugin.toml"),
            format!("{existing}\n[capabilities]\nhost_services = [{joined}]\n"),
        )?;
    }
    Ok(bundle)
}

fn agent_approver(workspace: &Path, posture: AgentPosture) -> PluginApprover {
    PluginApprover::Agent(AgentApproval {
        posture,
        workspace: workspace.to_string_lossy().into_owned(),
        thread_id: Some("thr_audit_1".to_string()),
        turn_id: Some("turn_audit_1".to_string()),
        tool_call_id: Some("call_audit_1".to_string()),
    })
}

fn approve_and_enable_by_agent(
    root: &Path,
    workspace: &Path,
    selector: &str,
    posture: AgentPosture,
) -> Result<()> {
    let discovery = isolated_plugin_discovery(root, workspace);
    let mut registry = (*discovery.registry_for_workspace(workspace)).clone();
    let approver = agent_approver(workspace, posture);
    let review = registry
        .host_service_review_for(selector, &approver)
        .ok_or_else(|| anyhow::anyhow!("missing review for {selector}"))?;
    registry
        .trust_and_enable_by(
            selector,
            ReviewApproval {
                approved_by: approver.clone(),
                host_services: Some(review.token),
            },
            approver,
        )
        .map_err(|error| anyhow::anyhow!(error))
}

fn approve_by_agent_only(
    root: &Path,
    workspace: &Path,
    selector: &str,
    posture: AgentPosture,
) -> Result<()> {
    let discovery = isolated_plugin_discovery(root, workspace);
    let mut registry = (*discovery.registry_for_workspace(workspace)).clone();
    let approver = agent_approver(workspace, posture);
    let review = registry
        .host_service_review_for(selector, &approver)
        .ok_or_else(|| anyhow::anyhow!("missing review for {selector}"))?;
    registry
        .trust_reviewed(
            selector,
            ReviewApproval {
                approved_by: approver,
                host_services: Some(review.token),
            },
        )
        .map_err(|error| anyhow::anyhow!(error))
}

async fn install_only(client: &reqwest::Client, base: &str, source: &Path) -> Result<String> {
    let response: serde_json::Value = client
        .post(format!("{base}/install"))
        .json(&serde_json::json!({ "source": source.display().to_string() }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(response["name"]
        .as_str()
        .expect("installed plugin name")
        .to_string())
}

#[tokio::test]
async fn plugins_list_and_detail_say_who_approved_and_who_turned_on() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    let sources = tmp.path().join("sources");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&sources)?;

    let agent_src = write_plugin_source_bundle(&sources, "agent-plug")?;
    let shell_src = write_plugin_source_bundle(&sources, "shell-plug")?;
    let cli_src = write_plugin_source_bundle(&sources, "cli-plug")?;
    let person_src = write_plugin_source_bundle(&sources, "person-plug")?;
    let mixed_src = write_plugin_source_bundle(&sources, "mixed-plug")?;
    let legacy_src = write_plugin_source_bundle(&sources, "legacy-plug")?;

    let Some((addr, handle)) = spawn_plugin_api_server(root.clone(), workspace.clone()).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");

    let agent_name = install_only(&client, &base, &agent_src).await?;
    approve_and_enable_by_agent(&root, &workspace, &agent_name, AgentPosture::AutoReview)?;

    let shell_name = install_only(&client, &base, &shell_src).await?;
    approve_and_enable_by_agent(&root, &workspace, &shell_name, AgentPosture::CommandLine)?;

    let cli_name = install_only(&client, &base, &cli_src).await?;
    {
        let discovery = isolated_plugin_discovery(&root, &workspace);
        let mut registry = (*discovery.registry_for_workspace(&workspace)).clone();
        let approver = PluginApprover::CommandLine;
        registry
            .trust_and_enable_by(
                &cli_name,
                ReviewApproval {
                    approved_by: approver.clone(),
                    host_services: None,
                },
                approver,
            )
            .map_err(|error| anyhow::anyhow!(error))?;
    }

    plugin_ui::install_active(&client, &base, person_src.display().to_string()).await?;

    let mixed_name = install_only(&client, &base, &mixed_src).await?;
    approve_by_agent_only(&root, &workspace, &mixed_name, AgentPosture::FullAccess)?;
    client
        .post(format!("{base}/{mixed_name}/enable"))
        .send()
        .await?
        .error_for_status()?;

    let legacy_name = install_only(&client, &base, &legacy_src).await?;
    {
        let discovery = isolated_plugin_discovery(&root, &workspace);
        let mut registry = (*discovery.registry_for_workspace(&workspace)).clone();
        registry
            .trust(&legacy_name)
            .map_err(|error| anyhow::anyhow!(error))?;
        let plugin_id = registry
            .get(&legacy_name)
            .expect("legacy plugin")
            .id
            .as_str()
            .to_string();
        let state_path = registry.state_path().expect("state path").to_path_buf();
        let mut state_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path)?)?;
        let entry = &mut state_json["plugins"][&plugin_id];
        entry["enabled"] = serde_json::json!(true);
        if let Some(obj) = entry.as_object_mut() {
            obj.remove("enabled_by");
        }
        if let Some(trust) = entry["trust"].as_object_mut() {
            trust.remove("approved_by");
        }
        if let Some(history) = entry["review_history"].as_array_mut() {
            for item in history {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("approved_by");
                }
            }
        }
        fs::write(&state_path, serde_json::to_string_pretty(&state_json)?)?;
    }

    let listed: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let plugins = listed["plugins"].as_array().expect("plugins array");
    let find_in_list = |name: &str| {
        plugins
            .iter()
            .find(|entry| entry["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("missing {name} in {listed}"))
    };

    let agent_summary = find_in_list("agent-plug");
    assert_eq!(agent_summary["approved_by"]["kind"], "agent");
    assert_eq!(agent_summary["approved_by"]["posture"], "auto_review");
    assert_eq!(agent_summary["enabled_by"]["kind"], "agent");
    assert_eq!(agent_summary["enabled_by"]["posture"], "auto_review");
    assert_eq!(agent_summary["cohort"]["kind"], "agent");
    assert_eq!(agent_summary["outside_reach"], false);

    let shell_summary = find_in_list("shell-plug");
    assert_eq!(shell_summary["approved_by"]["kind"], "agent");
    assert_eq!(shell_summary["approved_by"]["posture"], "command_line");
    assert_eq!(shell_summary["enabled_by"]["kind"], "agent");
    assert_eq!(shell_summary["enabled_by"]["posture"], "command_line");
    assert_eq!(shell_summary["cohort"]["kind"], "agent");

    let cli_summary = find_in_list("cli-plug");
    assert_eq!(
        cli_summary["approved_by"],
        serde_json::json!({ "kind": "command_line" })
    );
    assert_eq!(
        cli_summary["enabled_by"],
        serde_json::json!({ "kind": "command_line" })
    );
    assert_eq!(
        cli_summary["cohort"],
        serde_json::json!({ "kind": "reviewed" })
    );

    let legacy_summary = find_in_list("legacy-plug");
    assert!(legacy_summary["approved_by"].is_null());
    assert!(legacy_summary["enabled_by"].is_null());
    assert_eq!(
        legacy_summary["cohort"],
        serde_json::json!({ "kind": "reviewed" })
    );

    let person_summary = find_in_list("person-plug");
    assert_eq!(
        person_summary["approved_by"],
        serde_json::json!({ "kind": "person", "via": "app" })
    );
    assert_eq!(
        person_summary["enabled_by"],
        serde_json::json!({ "kind": "person", "via": "app" })
    );
    assert_eq!(
        person_summary["cohort"],
        serde_json::json!({ "kind": "reviewed" })
    );
    assert_eq!(person_summary["outside_reach"], false);

    let mixed_summary = find_in_list("mixed-plug");
    assert_eq!(mixed_summary["approved_by"]["kind"], "agent");
    assert_eq!(mixed_summary["approved_by"]["posture"], "full_access");
    assert_eq!(
        mixed_summary["enabled_by"],
        serde_json::json!({ "kind": "person", "via": "app" })
    );
    assert_eq!(mixed_summary["cohort"]["kind"], "agent");

    let agent_detail: serde_json::Value = client
        .get(format!("{base}/agent-plug"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(agent_detail["approved_by"], agent_summary["approved_by"]);
    assert_eq!(agent_detail["enabled_by"], agent_summary["enabled_by"]);
    assert_eq!(agent_detail["cohort"], agent_summary["cohort"]);
    let reviewed_at = agent_detail["reviewed_at"]
        .as_str()
        .expect("reviewed_at on detail");
    assert!(!reviewed_at.is_empty());
    let history = agent_detail["review_history"]
        .as_array()
        .expect("review_history array");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["reviewed_at"], reviewed_at);
    assert_eq!(history[0]["approved_by"], agent_summary["approved_by"]);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn a_person_review_removes_the_agent_label_and_keeps_the_history() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    let sources = tmp.path().join("sources");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&sources)?;

    let alpha_src = write_plugin_source_bundle(&sources, "alpha")?;
    let Some((addr, handle)) = spawn_plugin_api_server(root.clone(), workspace.clone()).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");

    let name = install_only(&client, &base, &alpha_src).await?;
    approve_and_enable_by_agent(&root, &workspace, &name, AgentPosture::FullAccess)?;

    let before_detail: serde_json::Value = client
        .get(format!("{base}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(before_detail["approved_by"]["kind"], "agent");
    assert_eq!(before_detail["cohort"]["kind"], "agent");
    let token = before_detail["review"]["token"]
        .as_str()
        .expect("review token")
        .to_string();

    client
        .post(format!("{base}/{name}/trust"))
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await?
        .error_for_status()?;

    let listed: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let summary = listed["plugins"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["name"] == "alpha"))
        .expect("alpha in list");
    assert_eq!(
        summary["approved_by"],
        serde_json::json!({ "kind": "person", "via": "app" })
    );
    assert_eq!(summary["cohort"], serde_json::json!({ "kind": "reviewed" }));
    assert_eq!(summary["outside_reach"], false);
    assert_eq!(summary["enabled"], false);
    assert!(summary["enabled_by"].is_null());

    let after_detail: serde_json::Value = client
        .get(format!("{base}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(
        after_detail["approved_by"],
        serde_json::json!({ "kind": "person", "via": "app" })
    );
    assert_eq!(
        after_detail["cohort"],
        serde_json::json!({ "kind": "reviewed" })
    );
    let history = after_detail["review_history"]
        .as_array()
        .expect("review_history");
    assert_eq!(history.len(), 2, "{history:?}");
    assert_eq!(
        history[0]["approved_by"],
        serde_json::json!({ "kind": "person", "via": "app" })
    );
    assert_eq!(history[1]["approved_by"]["kind"], "agent");
    assert_eq!(history[1]["approved_by"]["posture"], "full_access");
    assert_eq!(after_detail["reviewed_at"], history[0]["reviewed_at"]);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn an_agent_record_from_another_project_is_listed_as_on_only_there() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let here = tmp.path().join("here");
    let elsewhere = tmp.path().join("elsewhere");
    let sources = tmp.path().join("sources");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&here)?;
    fs::create_dir_all(&elsewhere)?;
    fs::create_dir_all(&sources)?;

    let alpha_src = write_plugin_source_bundle(&sources, "alpha")?;
    let Some((addr, handle)) = spawn_plugin_api_server(root.clone(), here.clone()).await? else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");

    let name = install_only(&client, &base, &alpha_src).await?;
    approve_and_enable_by_agent(&root, &elsewhere, &name, AgentPosture::FullAccess)?;

    let listed: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let summary = listed["plugins"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["name"] == "alpha"))
        .expect("alpha in list");
    assert_eq!(summary["enabled"], true);
    assert_eq!(summary["active"], false);
    assert_eq!(summary["outside_reach"], true);
    assert_eq!(summary["approved_by"]["kind"], "agent");
    assert_eq!(
        summary["approved_by"]["workspace"],
        elsewhere.to_string_lossy().as_ref()
    );
    assert_eq!(summary["cohort"]["kind"], "agent");
    assert_eq!(
        summary["cohort"]["workspace"],
        codewhale_config::normalize_project_workspace(&elsewhere)?
            .to_string_lossy()
            .as_ref()
    );

    let detail: serde_json::Value = client
        .get(format!("{base}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(detail["outside_reach"], true);
    assert_eq!(detail["active"], false);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn plugin_services_are_can_use_or_not_offered() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("runtime");
    let workspace = tmp.path().join("ws");
    let sources = tmp.path().join("sources");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&sources)?;

    let svc_src = write_bundle_with_host_services(&sources, "svc", &["workspace", "attachments"])?;
    let peer_src = write_plugin_source_bundle(&sources, "peer")?;
    fs::create_dir_all(peer_src.join("native"))?;
    fs::write(peer_src.join("native/index.mjs"), "export default {};\n")?;
    let peer_manifest = fs::read_to_string(peer_src.join("plugin.toml"))?;
    fs::write(
        peer_src.join("plugin.toml"),
        format!("{peer_manifest}\n[native]\npath = \"native/index.mjs\"\n"),
    )?;

    let Some((addr, handle)) = spawn_plugin_api_server(root.clone(), workspace.clone()).await?
    else {
        return Ok(());
    };
    let client = codewhale_release::tls::reqwest_client();
    let base = format!("http://{addr}/v1/apps/plugins");

    let name = install_only(&client, &base, &svc_src).await?;
    let before: serde_json::Value = client
        .get(format!("{base}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let before_services = before["host_services"]
        .as_array()
        .expect("host_services array before trust");
    assert_eq!(before_services.len(), 2, "{before_services:?}");
    for item in before_services {
        assert_eq!(item["state"], "not_offered", "{item}");
        assert!(item.get("withheld_by").is_none(), "{item}");
    }

    let token = before["review"]["token"].as_str().unwrap().to_string();
    let host_services_token = before["review"]["host_services"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    client
        .post(format!("{base}/{name}/trust"))
        .json(&serde_json::json!({
            "token": token,
            "host_services_token": host_services_token,
        }))
        .send()
        .await?
        .error_for_status()?;
    client
        .post(format!("{base}/{name}/enable"))
        .send()
        .await?
        .error_for_status()?;

    let active_detail: serde_json::Value = client
        .get(format!("{base}/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let active_services = active_detail["host_services"]
        .as_array()
        .expect("host_services array after trust");
    let find_service = |items: &[serde_json::Value], service: &str| {
        items
            .iter()
            .find(|item| item["service"] == service)
            .cloned()
            .unwrap_or_else(|| panic!("missing service {service} in {items:?}"))
    };
    let ws_before_peer = find_service(active_services, "workspace");
    assert_eq!(ws_before_peer["state"], "can_use");
    assert!(ws_before_peer.get("withheld_by").is_none());
    let att_before_peer = find_service(active_services, "attachments");
    assert_eq!(att_before_peer["state"], "not_offered");
    assert!(att_before_peer.get("withheld_by").is_none());

    plugin_ui::install_active(&client, &base, peer_src.display().to_string()).await?;
    let listed: serde_json::Value = client
        .get(&base)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let svc_row = listed["plugins"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "svc"))
        .expect("svc in list");
    let svc_services = svc_row["host_services"].as_array().expect("host_services");
    for item in svc_services {
        let state = item["state"].as_str().expect("string state");
        assert!(state == "can_use" || state == "not_offered", "{item}");
    }
    let ws_with_peer = find_service(svc_services, "workspace");
    assert_eq!(ws_with_peer["state"], "can_use");
    let withheld_by = ws_with_peer["withheld_by"]
        .as_array()
        .expect("withheld_by present when peer lacks grant");
    assert!(
        withheld_by.iter().any(|peer| peer["name"] == "peer"),
        "{withheld_by:?}"
    );

    handle.abort();
    Ok(())
}
