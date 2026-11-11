use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::activation::TestPolicyGuard;
use super::discovery::{DiscoveryConfig, discover_with_config};
use super::host_services::{
    AgentApproval, AgentPosture, HostService, PersonVia, PluginApprover, ReviewApproval, TestOffer,
};
use super::registry::{PluginRegistry, granted_host_services, verify_plugin_state_authority};

type Edit = fn(&mut Value);

fn config(root: &Path) -> DiscoveryConfig {
    DiscoveryConfig {
        workspace: root.join("project"),
        user_plugins_dir: root.join("user"),
        workspace_plugins_dir: root.join("workspace"),
        builtin_plugin_dirs: Vec::new(),
        state_path: root.join("state/plugin-state.json"),
    }
}

fn write_plugin(config: &DiscoveryConfig, name: &str, asks: &[&str], body: &str) {
    write_plugin_in(&config.user_plugins_dir, name, asks, body);
}

fn write_plugin_in(dir: &Path, name: &str, asks: &[&str], body: &str) {
    let plugin = dir.join(name);
    fs::create_dir_all(plugin.join("native")).unwrap();
    fs::write(plugin.join("native/index.mjs"), body).unwrap();
    let asked = if asks.is_empty() {
        String::new()
    } else {
        format!(
            "[capabilities]\nhost_services = [{}]\n",
            asks.iter()
                .map(|service| format!("{service:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    fs::write(
        plugin.join("plugin.toml"),
        format!(
            "schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n[native]\npath = \"native/index.mjs\"\n{asked}"
        ),
    )
    .unwrap();
}

fn person() -> PluginApprover {
    PluginApprover::person(PersonVia::Terminal)
}

fn agent(config: &DiscoveryConfig, posture: AgentPosture) -> PluginApprover {
    PluginApprover::Agent(AgentApproval {
        posture,
        workspace: config.workspace.to_string_lossy().into_owned(),
        thread_id: Some("thread-1".into()),
        turn_id: Some("turn-1".into()),
        tool_call_id: Some("call-1".into()),
    })
}

fn approve(registry: &mut PluginRegistry, name: &str, approved_by: PluginApprover) {
    let shown = registry
        .host_service_review_for(name, &approved_by)
        .filter(|_| approved_by != PluginApprover::CommandLine)
        .map(|review| review.token);
    registry
        .trust_reviewed(
            name,
            ReviewApproval {
                approved_by,
                host_services: shown,
            },
        )
        .unwrap();
    registry.enable(name).unwrap();
}

fn granted(config: &DiscoveryConfig, name: &str) -> Result<Vec<HostService>, String> {
    let registry = discover_with_config(config);
    let authority = registry
        .authority_for(name)
        .ok_or_else(|| "no authority".to_string())?;
    granted_host_services(&authority)
}

fn edit_receipt(config: &DiscoveryConfig, name: &str, edit: impl FnOnce(&mut Value)) {
    let mut state: Value =
        serde_json::from_str(&fs::read_to_string(&config.state_path).unwrap()).unwrap();
    let entry = state["plugins"]
        .as_object_mut()
        .unwrap()
        .iter_mut()
        .find(|(id, _)| id.ends_with(&format!("/{name}")))
        .map(|(_, entry)| entry)
        .unwrap();
    edit(&mut entry["trust"]);
    fs::write(
        &config.state_path,
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
}

#[test]
fn a_reviewed_grant_is_recorded_with_its_approver_and_read_back_for_the_reviewed_content() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network", "llm"],
        "export function apply() {}\n",
    );

    let mut registry = discover_with_config(&config);
    let review = registry.host_service_review("alpha").unwrap();
    assert_eq!(
        review
            .lines
            .iter()
            .map(|line| (line.service, line.offered))
            .collect::<Vec<_>>(),
        vec![(HostService::Llm, false), (HostService::Network, true)]
    );
    assert!(review.text()[1].contains("does not offer it"));
    assert!(review.text()[2].ends_with("No other reviewed plugin holds it."));
    let by_agent = agent(&config, AgentPosture::FullAccess);
    approve(&mut registry, "alpha", by_agent.clone());

    let plugin = registry.get("alpha").unwrap();
    assert_eq!(plugin.host_services, vec![HostService::Network]);
    assert_eq!(plugin.approved_by, Some(by_agent.clone()));
    assert_eq!(plugin.enabled_by, Some(person()));
    assert!(plugin.active());
    assert_eq!(
        granted(&config, "alpha").unwrap(),
        vec![HostService::Network]
    );
    let state: Value =
        serde_json::from_str(&fs::read_to_string(&config.state_path).unwrap()).unwrap();
    let receipt = &state["plugins"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["trust"];
    assert_eq!(receipt["version"], 2);
    assert_eq!(
        receipt["approved_by"],
        json!({
            "kind": "agent",
            "posture": "full_access",
            "workspace": config.workspace.to_string_lossy(),
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "tool_call_id": "call-1",
        })
    );
    assert_eq!(receipt["host_services"], json!(["network"]));
    assert_eq!(
        receipt["reviewed_capabilities"]["host_services"],
        json!(["llm", "network"])
    );

    write_plugin(
        &config,
        "alpha",
        &["network", "llm"],
        "export function apply() { globalThis.x = 1 }\n",
    );
    let changed = discover_with_config(&config);
    assert!(!changed.get("alpha").unwrap().trusted());
    assert!(changed.get("alpha").unwrap().host_services.is_empty());
    assert_eq!(changed.get("alpha").unwrap().approved_by, None);
    assert!(granted(&config, "alpha").is_err());
}

#[test]
fn trust_without_the_shown_review_grants_nothing_and_a_stale_token_refuses() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network"],
        "export function apply() {}\n",
    );
    write_plugin(
        &config,
        "beta",
        &["network"],
        "export function apply() {}\n",
    );
    write_plugin(&config, "plain", &[], "export function apply() {}\n");

    let mut registry = discover_with_config(&config);
    registry.trust("alpha").unwrap();
    registry.enable("alpha").unwrap();
    assert!(registry.get("alpha").unwrap().trusted());
    assert_eq!(registry.get("alpha").unwrap().approved_by, Some(person()));
    assert!(granted(&config, "alpha").unwrap().is_empty());
    let plain = registry.host_service_review("plain").unwrap();
    assert!(plain.lines.is_empty() && plain.text().is_empty());
    assert!(
        registry
            .trust_reviewed(
                "plain",
                ReviewApproval {
                    approved_by: person(),
                    host_services: Some("0".repeat(64)),
                },
            )
            .unwrap_err()
            .contains("changed since it was shown")
    );

    let sole = registry.host_service_review("beta").unwrap();
    assert!(sole.lines[0].shared_with.is_empty());
    assert_eq!(sole.lines[0].withheld_by[0].name, "alpha");
    assert!(
        sole.text()[1]
            .ends_with("Off for now: `alpha` is turned on and its review does not include it.")
    );
    approve(&mut registry, "alpha", person());
    let before = fs::read(&config.state_path).unwrap();
    let refused = registry
        .trust_reviewed(
            "beta",
            ReviewApproval {
                approved_by: person(),
                host_services: Some(sole.token.clone()),
            },
        )
        .unwrap_err();
    assert!(refused.contains("changed since it was shown"), "{refused}");
    assert_eq!(fs::read(&config.state_path).unwrap(), before);
    assert!(!registry.get("beta").unwrap().trusted());

    let shared = registry.host_service_review("beta").unwrap();
    assert_ne!(shared.token, sole.token);
    assert_eq!(shared.lines[0].shared_with[0].name, "alpha");
    assert!(shared.lines[0].withheld_by.is_empty());
    assert!(shared.text()[1].contains("Shared with `alpha`: each can use it as this plugin"));
    approve(&mut registry, "beta", person());
    assert_eq!(
        granted(&config, "beta").unwrap(),
        vec![HostService::Network]
    );
    assert_eq!(
        granted(&config, "alpha").unwrap(),
        vec![HostService::Network]
    );
    let again = registry.host_service_review("alpha").unwrap();
    assert_eq!(again.lines[0].shared_with[0].name, "beta");
}

#[test]
fn an_older_altered_or_unreadable_record_grants_nothing() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network, HostService::Llm]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network"],
        "export function apply() {}\n",
    );
    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", person());
    let authority = registry.authority_for("alpha").unwrap();
    assert_eq!(
        granted_host_services(&authority).unwrap(),
        vec![HostService::Network]
    );
    let sound = fs::read(&config.state_path).unwrap();

    edit_receipt(&config, "alpha", |receipt| {
        receipt.as_object_mut().unwrap().remove("version");
        receipt.as_object_mut().unwrap().remove("approved_by");
    });
    assert!(verify_plugin_state_authority(&authority).is_ok());
    assert!(granted_host_services(&authority).unwrap().is_empty());
    let older = discover_with_config(&config);
    assert!(older.get("alpha").unwrap().trusted());
    assert_eq!(older.get("alpha").unwrap().approved_by, None);
    assert!(older.get("alpha").unwrap().host_services.is_empty());

    let edits: [(&str, Edit); 3] = [
        ("a later format", |receipt| receipt["version"] = json!(3)),
        ("a grant beyond what was asked", |receipt| {
            receipt["host_services"] = json!(["network", "llm"]);
        }),
        ("an edited reviewed inventory", |receipt| {
            receipt["host_services"] = json!(["network", "llm"]);
            receipt["reviewed_capabilities"]["host_services"] = json!(["llm", "network"]);
        }),
    ];
    for (what, edit) in edits {
        fs::write(&config.state_path, &sound).unwrap();
        edit_receipt(&config, "alpha", edit);
        assert!(
            granted_host_services(&authority).unwrap().is_empty(),
            "{what}"
        );
        assert!(
            discover_with_config(&config)
                .get("alpha")
                .unwrap()
                .host_services
                .is_empty(),
            "{what}"
        );
    }

    fs::write(&config.state_path, &sound).unwrap();
    edit_receipt(&config, "alpha", |receipt| {
        receipt["capability_hash"] = json!("0".repeat(64));
    });
    assert!(granted_host_services(&authority).is_err());

    fs::write(&config.state_path, &sound).unwrap();
    edit_receipt(&config, "alpha", |receipt| {
        receipt["host_services"] = json!(["everything"]);
    });
    assert!(granted_host_services(&authority).is_err());
    assert!(
        !discover_with_config(&config)
            .get("alpha")
            .unwrap()
            .trusted()
    );

    fs::write(&config.state_path, b"{ not json").unwrap();
    assert!(granted_host_services(&authority).is_err());
    fs::remove_file(&config.state_path).unwrap();
    assert!(granted_host_services(&authority).is_err());

    fs::write(&config.state_path, &sound).unwrap();
    assert_eq!(
        granted_host_services(&authority).unwrap(),
        vec![HostService::Network]
    );
}

#[test]
fn turning_off_revoking_or_reviewing_again_withdraws_the_grant_from_the_old_authority() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network"],
        "export function apply() {}\n",
    );
    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", person());
    let held = registry.authority_for("alpha").unwrap();
    assert_eq!(
        granted_host_services(&held).unwrap(),
        vec![HostService::Network]
    );

    discover_with_config(&config).disable("alpha").unwrap();
    assert!(granted_host_services(&held).is_err());
    let mut registry = discover_with_config(&config);
    registry.enable("alpha").unwrap();
    assert!(
        granted_host_services(&held).is_err(),
        "an authority from before the plugin was turned off is spent"
    );
    let current = registry.authority_for("alpha").unwrap();
    assert_eq!(
        granted_host_services(&current).unwrap(),
        vec![HostService::Network]
    );

    discover_with_config(&config).revoke_trust("alpha").unwrap();
    assert!(granted_host_services(&current).is_err());

    let mut registry = discover_with_config(&config);
    registry.trust("alpha").unwrap();
    registry.enable("alpha").unwrap();
    assert!(granted_host_services(&current).is_err());
    assert!(
        granted_host_services(&registry.authority_for("alpha").unwrap())
            .unwrap()
            .is_empty(),
        "a review that did not show the services grants none"
    );

    write_plugin(&config, "alpha", &[], "export function apply() {}\n");
    let narrowed = discover_with_config(&config);
    assert_eq!(
        narrowed.get("alpha").unwrap().trust_status.as_str(),
        "capabilities-changed"
    );
    assert!(narrowed.authority_for("alpha").is_none() || granted(&config, "alpha").is_err());
}

#[test]
fn the_importer_asks_only_for_services_its_own_server_modules_inject() {
    use std::collections::BTreeMap;

    use super::install::dsh::{DshClientClosure, asked_host_services};

    let table = [
        ("llm", HostService::Llm),
        ("credentials", HostService::Network),
        ("remote", HostService::Remote),
        ("terminal", HostService::Terminal),
    ];
    let file = |text: &str| text.as_bytes().to_vec();
    let files: BTreeMap<String, Vec<u8>> = [
        (
            "source/lib/index.js",
            file("export const inject = ['tools', 'llm']"),
        ),
        (
            "source/lib/client.js",
            file("export const inject = [\"slots\", \"remote\"]"),
        ),
        ("source/README.md", file("inject = ['terminal']")),
        (
            "source/node_modules/@scope/row/index.mjs",
            file("export const inject = ['credentials']"),
        ),
        (
            "source/node_modules/@scope/row/node_modules/deep/index.js",
            file("export const inject = ['terminal']"),
        ),
        (
            "source/node_modules/other/index.js",
            file("export const inject = ['terminal']"),
        ),
        (
            "native/index.mjs",
            file("export const inject = ['terminal']"),
        ),
    ]
    .into_iter()
    .map(|(path, bytes)| (path.to_string(), bytes))
    .collect();
    let client = DshClientClosure {
        package: "demo".into(),
        root: String::new(),
        entry: "source/lib/client.js".into(),
        platform: None,
        inject: vec!["remote".into()],
        host_provided: Vec::new(),
        files: [("source/lib/client.js".to_string(), String::new())].into(),
        bytes: 0,
    };
    let rows = vec!["demo".to_string(), "@scope/row".to_string()];
    assert_eq!(
        asked_host_services(&files, &rows, std::slice::from_ref(&client), &table),
        vec![HostService::Llm, HostService::Network]
    );
    assert_eq!(
        asked_host_services(&files, &rows, &[], &table),
        vec![HostService::Llm, HostService::Network, HostService::Remote]
    );
    assert!(asked_host_services(&files, &rows, &[], &[]).is_empty());
    assert!(
        asked_host_services(
            &files,
            &rows,
            &[],
            super::host_services::HOST_SERVICE_INJECTS
        )
        .is_empty()
    );
}

#[test]
fn the_agent_is_an_approver_only_with_the_token_of_its_own_review_and_in_its_own_project() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network"],
        "export function apply() {}\n",
    );
    write_plugin(
        &config,
        "beta",
        &["network"],
        "export function apply() {}\n",
    );
    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", person());
    let by_agent = agent(&config, AgentPosture::FullAccess);
    let agents_review = registry.host_service_review_for("beta", &by_agent).unwrap();
    let persons_review = registry.host_service_review("beta").unwrap();
    assert!(agents_review.lines[0].shared_with.is_empty());
    assert_eq!(persons_review.lines[0].shared_with[0].name, "alpha");
    assert_ne!(agents_review.token, persons_review.token);

    let before = fs::read(&config.state_path).unwrap();
    let PluginApprover::Agent(fields) = by_agent.clone() else {
        unreachable!()
    };
    let refusals = [
        (by_agent.clone(), None, "must carry the token"),
        (
            PluginApprover::Agent(AgentApproval {
                tool_call_id: None,
                ..fields.clone()
            }),
            Some(agents_review.token.clone()),
            "must name the thread, turn and tool call",
        ),
        (
            PluginApprover::Agent(AgentApproval {
                workspace: tmp.path().join("elsewhere").to_string_lossy().into_owned(),
                ..fields.clone()
            }),
            Some(agents_review.token.clone()),
            "limited to its own project",
        ),
        (
            by_agent.clone(),
            Some(persons_review.token.clone()),
            "changed since it was shown",
        ),
    ];
    for (approved_by, host_services, says) in refusals {
        let refused = registry
            .trust_reviewed(
                "beta",
                ReviewApproval {
                    approved_by,
                    host_services,
                },
            )
            .unwrap_err();
        assert!(refused.contains(says), "{refused}");
        assert_eq!(fs::read(&config.state_path).unwrap(), before);
    }

    approve(&mut registry, "beta", by_agent.clone());
    assert_eq!(registry.get("beta").unwrap().approved_by, Some(by_agent));
    assert_eq!(
        granted(&config, "beta").unwrap(),
        vec![HostService::Network]
    );
    let alphas = registry.host_service_review("alpha").unwrap();
    assert!(
        alphas.lines[0].shared_with.is_empty(),
        "a reviewed plugin shares nothing with an agent-approved one"
    );
}

#[test]
fn an_agent_approved_record_is_active_only_in_the_project_it_names() {
    let _policy = TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "alpha", &[], "export function apply() {}\n");
    write_plugin(&config, "beta", &[], "export function apply() {}\n");
    let mut registry = discover_with_config(&config);
    approve(
        &mut registry,
        "alpha",
        agent(&config, AgentPosture::AutoReview),
    );
    approve(&mut registry, "beta", person());
    assert!(registry.get("alpha").unwrap().active());

    let elsewhere = DiscoveryConfig {
        workspace: tmp.path().join("other-project"),
        ..config.clone()
    };
    let other = discover_with_config(&elsewhere);
    let alpha = other.get("alpha").unwrap();
    assert!(alpha.trusted() && alpha.enabled && alpha.outside_reach);
    assert!(!alpha.active());
    assert!(other.get("beta").unwrap().active());

    let again = discover_with_config(&config);
    assert!(!again.get("alpha").unwrap().outside_reach);
    assert!(again.get("alpha").unwrap().active());
}

#[test]
fn a_command_line_trust_is_recorded_as_such_and_the_agents_mark_makes_it_the_agents() {
    let _policy = TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "alpha", &[], "export function apply() {}\n");
    write_plugin(&config, "beta", &[], "export function apply() {}\n");
    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", PluginApprover::command_line(None));
    let marked = PluginApprover::command_line(Some(&config.workspace));
    assert!(
        registry
            .trust_reviewed(
                "beta",
                ReviewApproval {
                    approved_by: marked.clone(),
                    host_services: None,
                },
            )
            .unwrap_err()
            .contains("must carry the token")
    );
    approve(&mut registry, "beta", marked.clone());

    let state: Value =
        serde_json::from_str(&fs::read_to_string(&config.state_path).unwrap()).unwrap();
    let record = |name: &str| {
        state["plugins"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(id, _)| id.ends_with(&format!("/{name}")))
            .map(|(_, entry)| entry.clone())
            .unwrap()
    };
    assert_eq!(
        record("alpha")["trust"]["approved_by"],
        json!({"kind": "command_line"})
    );
    assert_eq!(
        record("beta")["trust"]["approved_by"],
        json!({"kind": "agent", "posture": "command_line", "workspace": config.workspace.to_string_lossy()})
    );
    assert_eq!(
        record("beta")["enabled_by"],
        json!({"kind": "person", "via": "terminal"})
    );

    let mut registry = discover_with_config(&config);
    assert!(
        registry
            .enable_by("alpha", marked.clone())
            .unwrap_err()
            .contains("approved for every project")
    );
    registry.disable("beta").unwrap();
    registry.enable_by("beta", marked.clone()).unwrap();
    assert_eq!(registry.get("beta").unwrap().enabled_by, Some(marked));
    registry.disable("alpha").unwrap();
    assert_eq!(registry.get("alpha").unwrap().enabled_by, None);
    registry.enable("alpha").unwrap();
    registry.trust("alpha").unwrap();
    let alpha = registry.get("alpha").unwrap();
    assert!(!alpha.enabled && alpha.enabled_by.is_none());

    let elsewhere = DiscoveryConfig {
        workspace: tmp.path().join("other-project"),
        ..config.clone()
    };
    assert!(
        !discover_with_config(&elsewhere)
            .get("beta")
            .unwrap()
            .active()
    );
}

#[test]
fn the_agent_turns_on_only_a_plugin_it_approved_in_its_own_project() {
    let _policy = TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "alpha", &[], "export function apply() {}\n");
    write_plugin(&config, "beta", &[], "export function apply() {}\n");
    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", person());
    approve(
        &mut registry,
        "beta",
        agent(&config, AgentPosture::FullAccess),
    );
    registry.disable("alpha").unwrap();
    registry.disable("beta").unwrap();
    let by_agent = agent(&config, AgentPosture::FullAccess);
    let PluginApprover::Agent(fields) = by_agent.clone() else {
        unreachable!()
    };
    let elsewhere = DiscoveryConfig {
        workspace: tmp.path().join("other-project"),
        ..config.clone()
    };
    let mut other = discover_with_config(&elsewhere);
    let before = fs::read(&config.state_path).unwrap();
    let refusals = [
        (
            &mut registry,
            "alpha",
            by_agent.clone(),
            "approved for every project",
        ),
        (
            &mut discover_with_config(&config),
            "beta",
            PluginApprover::Agent(AgentApproval {
                workspace: tmp.path().join("elsewhere").to_string_lossy().into_owned(),
                ..fields.clone()
            }),
            "limited to its own project",
        ),
        (
            &mut discover_with_config(&config),
            "beta",
            PluginApprover::Agent(AgentApproval {
                tool_call_id: None,
                ..fields.clone()
            }),
            "must name the thread, turn and tool call",
        ),
        (
            &mut other,
            "beta",
            agent(&elsewhere, AgentPosture::FullAccess),
            "agent of another project",
        ),
    ];
    for (registry, name, enabled_by, says) in refusals {
        let refused = registry.enable_by(name, enabled_by).unwrap_err();
        assert!(refused.contains(says), "{name}: {refused}");
        assert_eq!(fs::read(&config.state_path).unwrap(), before);
    }

    registry.enable_by("beta", by_agent.clone()).unwrap();
    let beta = registry.get("beta").unwrap();
    assert!(beta.active());
    assert_eq!(beta.enabled_by, Some(by_agent));
    assert!(
        !discover_with_config(&elsewhere)
            .get("beta")
            .unwrap()
            .active()
    );
}

#[test]
fn the_agent_never_replaces_a_review_held_for_every_project() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = DiscoveryConfig {
        builtin_plugin_dirs: vec![tmp.path().join("builtin")],
        ..config(tmp.path())
    };
    for name in ["alpha", "beta", "gamma"] {
        write_plugin(&config, name, &["network"], "export function apply() {}\n");
    }
    write_plugin_in(
        &config.builtin_plugin_dirs[0],
        "delta",
        &[],
        "export function apply() {}\n",
    );
    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", person());
    approve(&mut registry, "beta", PluginApprover::CommandLine);
    approve(&mut registry, "gamma", person());
    edit_receipt(&config, "gamma", |receipt| {
        receipt.as_object_mut().unwrap().remove("version");
        receipt.as_object_mut().unwrap().remove("approved_by");
    });
    let mut registry = discover_with_config(&config);
    assert_eq!(registry.get("gamma").unwrap().approved_by, None);
    assert!(registry.get("gamma").unwrap().trusted());
    let by_agent = agent(&config, AgentPosture::FullAccess);

    let before = fs::read(&config.state_path).unwrap();
    for (name, says) in [
        ("alpha", "holds a review for every project"),
        ("beta", "holds a review for every project"),
        ("gamma", "holds a review for every project"),
        ("delta", "built-in plugin"),
    ] {
        let shown = registry
            .host_service_review_for(name, &by_agent)
            .map(|review| review.token);
        let refused = registry
            .trust_reviewed(
                name,
                ReviewApproval {
                    approved_by: by_agent.clone(),
                    host_services: shown,
                },
            )
            .unwrap_err();
        assert!(refused.contains(says), "{name}: {refused}");
        assert_eq!(fs::read(&config.state_path).unwrap(), before, "{name}");
    }
    let refused = registry.enable_by("alpha", by_agent.clone()).unwrap_err();
    assert!(refused.contains("approved for every project"), "{refused}");
    let alpha = discover_with_config(&config);
    let alpha = alpha.get("alpha").unwrap();
    assert_eq!(alpha.approved_by, Some(person()));
    assert!(alpha.active());
    assert_eq!(
        granted(&config, "alpha").unwrap(),
        vec![HostService::Network]
    );
}

#[test]
fn the_agent_cannot_replace_a_review_made_after_its_registry_was_read() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network"],
        "export function apply() {}\n",
    );
    let mut stale = discover_with_config(&config);
    let mut current = discover_with_config(&config);
    approve(&mut current, "alpha", person());
    let before = fs::read(&config.state_path).unwrap();

    let by_agent = agent(&config, AgentPosture::FullAccess);
    let shown = stale
        .host_service_review_for("alpha", &by_agent)
        .map(|review| review.token);
    let refused = stale
        .trust_reviewed(
            "alpha",
            ReviewApproval {
                approved_by: by_agent,
                host_services: shown,
            },
        )
        .unwrap_err();
    assert!(refused.contains("state or review changed"), "{refused}");
    assert_eq!(fs::read(&config.state_path).unwrap(), before);
    let now = discover_with_config(&config);
    let alpha = now.get("alpha").unwrap();
    assert_eq!(alpha.approved_by, Some(person()));
    assert!(alpha.active());
    assert_eq!(
        granted(&config, "alpha").unwrap(),
        vec![HostService::Network]
    );
}

#[test]
fn the_agent_of_another_project_cannot_take_over_an_agents_record() {
    let _policy = TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "alpha", &[], "export function apply() {}\n");
    let mut registry = discover_with_config(&config);
    let home_agent = agent(&config, AgentPosture::FullAccess);
    approve(&mut registry, "alpha", home_agent.clone());
    let before = fs::read(&config.state_path).unwrap();

    let elsewhere = DiscoveryConfig {
        workspace: tmp.path().join("other-project"),
        ..config.clone()
    };
    let mut other = discover_with_config(&elsewhere);
    assert!(other.get("alpha").unwrap().outside_reach);
    let other_agent = agent(&elsewhere, AgentPosture::FullAccess);
    let shown = other
        .host_service_review_for("alpha", &other_agent)
        .map(|review| review.token);
    let refused = other
        .trust_reviewed(
            "alpha",
            ReviewApproval {
                approved_by: other_agent,
                host_services: shown,
            },
        )
        .unwrap_err();
    assert!(refused.contains("agent of another project"), "{refused}");
    assert_eq!(fs::read(&config.state_path).unwrap(), before);
    let home = discover_with_config(&config);
    let alpha = home.get("alpha").unwrap();
    assert_eq!(alpha.approved_by, Some(home_agent));
    assert!(alpha.active());
}

#[cfg(unix)]
#[test]
fn a_project_plugin_folder_that_resolves_outside_the_project_is_not_the_agents() {
    let _policy = TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let project = |name: &str| {
        let workspace = tmp.path().join(name);
        fs::create_dir_all(&workspace).unwrap();
        DiscoveryConfig {
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            workspace,
            ..config(tmp.path())
        }
    };
    let linked = project("linked");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(outside.join("plugins")).unwrap();
    std::os::unix::fs::symlink(&outside, linked.workspace.join(".codewhale")).unwrap();
    write_plugin_in(
        &linked.workspace_plugins_dir,
        "alpha",
        &[],
        "export function apply() {}\n",
    );
    let own = project("own");
    write_plugin_in(
        &own.workspace_plugins_dir,
        "beta",
        &[],
        "export function apply() {}\n",
    );

    let mut registry = discover_with_config(&linked);
    let alpha = registry.get("alpha").unwrap();
    assert_eq!(alpha.scope, super::types::PluginScope::Workspace);
    assert!(
        !alpha
            .canonical_root
            .starts_with(fs::canonicalize(&linked.workspace).unwrap())
    );
    let by_agent = agent(&linked, AgentPosture::FullAccess);
    let shown = registry
        .host_service_review_for("alpha", &by_agent)
        .map(|review| review.token);
    let refused = registry
        .trust_reviewed(
            "alpha",
            ReviewApproval {
                approved_by: by_agent,
                host_services: shown,
            },
        )
        .unwrap_err();
    assert!(
        refused.contains("resolves outside this project"),
        "{refused}"
    );
    assert!(registry.get("alpha").unwrap().approved_by.is_none());

    let mut registry = discover_with_config(&own);
    let by_agent = agent(&own, AgentPosture::FullAccess);
    let shown = registry
        .host_service_review_for("beta", &by_agent)
        .map(|review| review.token);
    registry
        .trust_reviewed(
            "beta",
            ReviewApproval {
                approved_by: by_agent.clone(),
                host_services: shown,
            },
        )
        .unwrap();
    registry.enable_by("beta", by_agent).unwrap();
    assert!(registry.get("beta").unwrap().active());
}

#[test]
fn a_command_line_approval_carries_no_host_service_grant() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(
        &config,
        "alpha",
        &["network"],
        "export function apply() {}\n",
    );
    let mut registry = discover_with_config(&config);
    let shown = registry
        .host_service_review_for("alpha", &PluginApprover::CommandLine)
        .map(|review| review.token);
    assert!(shown.is_some());
    let refused = registry
        .trust_reviewed(
            "alpha",
            ReviewApproval {
                approved_by: PluginApprover::CommandLine,
                host_services: shown,
            },
        )
        .unwrap_err();
    assert!(refused.contains("cannot grant host services"), "{refused}");
    assert!(registry.get("alpha").unwrap().approved_by.is_none());

    approve(&mut registry, "alpha", PluginApprover::CommandLine);
    assert_eq!(
        registry.get("alpha").unwrap().approved_by,
        Some(PluginApprover::CommandLine)
    );
    assert_eq!(granted(&config, "alpha").unwrap(), Vec::new());
}

mod v0_10_2 {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PluginStateFile {
        pub schema_version: u32,
        #[serde(default)]
        pub plugins: BTreeMap<String, PersistedPluginState>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PersistedPluginState {
        #[serde(default)]
        pub generation: u64,
        #[serde(default)]
        pub enabled: bool,
        #[serde(default)]
        pub trust: Option<TrustReceipt>,
        #[serde(default)]
        pub review_history: Vec<TrustReceipt>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrustReceipt {
        pub content_hash: String,
        pub capability_hash: String,
        pub reviewed_capabilities: PluginInventory,
        pub reviewed_at: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PluginInventory {
        pub skills: usize,
        pub mcp_servers: usize,
        #[serde(default)]
        pub stdio_mcp_servers: usize,
        #[serde(default)]
        pub remote_mcp_servers: usize,
        pub commands: usize,
        pub agents: usize,
        pub hooks: usize,
        pub lsp: usize,
        pub native: usize,
        #[serde(default)]
        pub providers: usize,
        pub filesystem_roots: Vec<String>,
        pub network_hosts: Vec<String>,
        pub lifecycle_mutation: bool,
    }

    impl PluginStateFile {
        pub fn reviews(&self) -> Vec<String> {
            self.plugins
                .values()
                .flat_map(|entry| {
                    entry
                        .trust
                        .iter()
                        .chain(&entry.review_history)
                        .map(move |receipt| {
                            format!(
                                "{}:{}:{}:{}:{}",
                                entry.generation,
                                entry.enabled,
                                receipt.content_hash,
                                receipt.capability_hash,
                                receipt.reviewed_at
                            ) + &receipt.reviewed_capabilities.counted()
                        })
                })
                .collect()
        }
    }

    impl PluginInventory {
        fn counted(&self) -> String {
            format!(
                ":{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                self.skills,
                self.mcp_servers,
                self.stdio_mcp_servers,
                self.remote_mcp_servers,
                self.commands,
                self.agents,
                self.hooks,
                self.lsp,
                self.native,
                self.providers,
                self.filesystem_roots.len(),
                self.network_hosts.len(),
                self.lifecycle_mutation
            )
        }
    }
}

#[test]
fn v0_10_2_reads_this_state_file_until_a_plugin_is_reviewed_or_turned_on_here() {
    let _policy = TestPolicyGuard::extension_host(true);
    let tmp = tempfile::tempdir().unwrap();
    let config = config(tmp.path());
    write_plugin(&config, "alpha", &[], "export function apply() {}\n");
    write_plugin(&config, "beta", &[], "export function apply() {}\n");
    let older = |config: &DiscoveryConfig| {
        serde_json::from_str::<v0_10_2::PluginStateFile>(
            &fs::read_to_string(&config.state_path).unwrap(),
        )
    };

    let mut registry = discover_with_config(&config);
    approve(&mut registry, "alpha", person());
    edit_receipt(&config, "alpha", |receipt| {
        receipt.as_object_mut().unwrap().remove("version");
        receipt.as_object_mut().unwrap().remove("approved_by");
    });
    let mut state: Value =
        serde_json::from_str(&fs::read_to_string(&config.state_path).unwrap()).unwrap();
    for entry in state["plugins"].as_object_mut().unwrap().values_mut() {
        entry.as_object_mut().unwrap().remove("enabled_by");
        for receipt in entry["review_history"].as_array_mut().unwrap() {
            receipt.as_object_mut().unwrap().remove("version");
            receipt.as_object_mut().unwrap().remove("approved_by");
        }
    }
    fs::write(
        &config.state_path,
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
    let parsed = older(&config).expect("a state file of v0.10.2's shape");
    assert_eq!(parsed.schema_version, 1);
    let content = &registry.get("alpha").unwrap().content_hash;
    assert!(
        parsed
            .reviews()
            .iter()
            .any(|review| review.contains(content.as_str())),
        "{:?}",
        parsed.reviews()
    );

    let mut registry = discover_with_config(&config);
    assert!(registry.get("alpha").unwrap().active());
    registry.disable("beta").unwrap();
    registry.disable("alpha").unwrap();
    let parsed = older(&config).expect("writes that record no review or enablement stay readable");
    assert!(parsed.plugins.values().all(|entry| !entry.enabled));

    registry.enable("alpha").unwrap();
    let refused = older(&config).unwrap_err().to_string();
    assert!(refused.contains("unknown field `enabled_by`"), "{refused}");

    registry.disable("alpha").unwrap();
    assert!(older(&config).is_ok());
    registry.trust("beta").unwrap();
    let refused = older(&config).unwrap_err().to_string();
    assert!(refused.contains("unknown field `version`"), "{refused}");
}
