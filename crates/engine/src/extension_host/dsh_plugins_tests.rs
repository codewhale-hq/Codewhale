use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use codewhale_config::{AppMode, OwnerRef};
use codewhale_execpolicy::ApprovalMode;
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::host_services::{
    AgentApproval, AgentPosture, HostService, PersonVia, PluginApprover, ReviewApproval, TestOffer,
};
use codewhale_skill_install::install::INSTALLED_FROM_MARKER;
use serde_json::{Value, json};

use super::super::core_call::{BrokerContext, InvocationGuard};
use super::super::dsh_broker::{BrokerActTool, TABLE};
use super::super::protocol::{DshBrokerParams, DshService, RpcErrorWire, error_code};
use super::super::registry::OwnerState;
use super::super::supervisor::HostRequestContext;
use super::super::test_support::fake_authority;
use super::super::tier::HostCohort;
use super::super::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use super::{MAX_TEXT_BYTES, clean_diagnostic_fragment, diagnostic_path, sanitize_inline, serve};
use crate::core::engine::{HumanDecision, LivePosture};
use crate::rlm::session::SessionObjectSnapshot;
use crate::tools::codemode::{ExtensionCaller, NestedCallGate, NestedCallVerdict, NestedDecision};
use crate::tools::spec::{ToolContext, ToolError};

struct PluginsFixture {
    _temp: tempfile::TempDir,
    config: DiscoveryConfig,
    manager: Arc<ExtensionHostManager>,
    creator: OwnerRef,
    mcp: OwnerRef,
    plugin_owner: OwnerRef,
    context: ToolContext,
    gate: NestedCallGate,
    posture: LivePosture,
    asked: Arc<AtomicUsize>,
}

impl PluginsFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("project");
        let user_plugins_dir = temp.path().join("user");
        let workspace_plugins_dir = workspace.join(".codewhale/plugins");
        let builtin_dir = temp.path().join("builtin");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&user_plugins_dir).unwrap();
        fs::create_dir_all(&workspace_plugins_dir).unwrap();
        fs::create_dir_all(&builtin_dir).unwrap();
        let config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir,
            workspace_plugins_dir,
            builtin_plugin_dirs: vec![builtin_dir],
            state_path: temp.path().join("state/plugin-state.json"),
        };

        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions::default()));
        let (creator, mcp, plugin_owner) = {
            let mut registry = manager.shared.registry.lock().unwrap();
            let creator = registry
                .begin_owner(
                    HostCohort::Builtin,
                    "host:creator",
                    "creator",
                    None,
                    "digest",
                )
                .unwrap();
            registry.mark_active(&creator);
            let mcp = registry
                .begin_owner(HostCohort::Builtin, "host:mcp", "mcp", None, "digest")
                .unwrap();
            registry.mark_active(&mcp);
            let plugin_owner = registry
                .begin_owner(
                    HostCohort::reviewed(),
                    "user/hash/alpha",
                    "alpha",
                    Some(fake_authority("alpha")),
                    "hash",
                )
                .unwrap();
            registry.mark_active(&plugin_owner);
            (creator, mcp, plugin_owner)
        };

        let asked = Arc::new(AtomicUsize::new(0));
        let asked_in_gate = Arc::clone(&asked);
        let gate = NestedCallGate::answering_for_test(move |name, input| {
            asked_in_gate.fetch_add(1, Ordering::SeqCst);
            NestedCallVerdict::Run {
                name: name.to_string(),
                input: input.clone(),
                supports_parallel: false,
                decision: NestedDecision::Auto,
                hook_context: None,
            }
        })
        .for_extension(
            ExtensionCaller {
                origin: "extension:creator".to_string(),
                tool: "creator_tool".to_string(),
                scope: "ext:host:creator@digest".to_string(),
            },
            BrokerActTool::all().to_vec(),
        );

        let posture = LivePosture::for_tests(&workspace, ApprovalMode::Suggest);
        let mut context = ToolContext::new(&workspace)
            .with_session_objects(SessionObjectSnapshot::new(
                "plugins-session".into(),
                "fixture-model".into(),
                workspace,
                None,
                Arc::new(Vec::new()),
            ))
            .with_origin_turn_id("plugins-turn")
            .with_origin_tool_call_id("plugins-tool-call");
        context.live_posture = Some(posture.clone());

        Self {
            _temp: temp,
            config,
            manager,
            creator,
            mcp,
            plugin_owner,
            context,
            gate,
            posture,
            asked,
        }
    }

    fn reload_registry(&mut self) -> Arc<PluginRegistry> {
        let registry = Arc::new(discover_with_config(&self.config));
        self.context = self
            .context
            .clone()
            .with_plugin_registry(Arc::clone(&registry));
        registry
    }

    fn begin(
        &self,
        owner: &OwnerRef,
        cohort: HostCohort,
        context: &ToolContext,
    ) -> InvocationGuard {
        let caller = self.gate.extension().unwrap().0.clone();
        let services = self.manager.shared.broker_services(owner);
        self.manager
            .shared
            .core_calls
            .begin_scoped(
                cohort,
                7,
                owner,
                "call-1",
                &caller,
                context,
                &self.gate,
                None,
                String::new(),
                &services,
            )
            .expect("an invocation")
    }

    async fn call_broker(
        &self,
        owner: &OwnerRef,
        cohort: HostCohort,
        ticket: &str,
        method: &str,
        input: Value,
    ) -> Result<Value, RpcErrorWire> {
        let (cx, _violations, _cancel) = HostRequestContext::for_test(100);
        self.manager
            .shared
            .core_calls
            .serve_broker_in(
                TABLE,
                &self.manager.shared,
                cohort,
                7,
                DshBrokerParams {
                    owner: owner.clone(),
                    ticket: ticket.to_string(),
                    service: "plugins".to_string(),
                    method: method.to_string(),
                    input,
                },
                cx,
            )
            .await
    }
}

fn plugin_ticket(guard: &InvocationGuard) -> Option<String> {
    guard
        .broker_tickets()
        .unwrap_or_default()
        .into_iter()
        .find(|wire| wire.service == "plugins")
        .map(|wire| wire.ticket)
}

fn write_toml_plugin(root: &Path, dir_name: &str, toml_body: &str) -> PathBuf {
    let plugin = root.join(dir_name);
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("plugin.toml"), toml_body).unwrap();
    plugin
}

fn write_skill(plugin: &Path, name: &str) {
    let dir = plugin.join("skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\nbody\n"),
    )
    .unwrap();
}

fn approve_reviewed(registry: &mut PluginRegistry, selector: &str, approver: PluginApprover) {
    let token = if matches!(approver, PluginApprover::CommandLine) {
        None
    } else {
        registry
            .host_service_review_for(selector, &approver)
            .map(|review| review.token)
    };
    registry
        .trust_reviewed(
            selector,
            ReviewApproval {
                approved_by: approver,
                host_services: token,
            },
        )
        .unwrap();
}

fn refusal(result: &Result<Value, RpcErrorWire>) -> (i64, String) {
    let error = result.as_ref().expect_err("expected refusal");
    let code = error
        .data
        .as_ref()
        .and_then(|data| data["dsh_refusal"].as_str())
        .unwrap_or_else(|| panic!("missing dsh_refusal: {error:?}"))
        .to_string();
    (error.code, code)
}

fn detached_call(
    owner: OwnerRef,
    method: &str,
    input: Value,
) -> super::super::dsh_broker::BrokerCall {
    super::super::dsh_broker::BrokerCall {
        context: BrokerContext::detached_for_test(&owner, DshService::Plugins),
        owner,
        session_id: "detached-session".into(),
        service: DshService::Plugins,
        method: method.into(),
        input,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn list_has_one_entry_per_plugin_and_says_who_approved() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let mut f = PluginsFixture::new();

    let person_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "alpha",
        "schema_version = 1\n[plugin]\nname = \"alpha\"\nversion = \"1.2.3\"\ndescription = \"Alpha plugin\"\n[skills]\npath = \"skills\"\n[capabilities]\nhost_services = [\"network\", \"llm\"]\n",
    );
    write_skill(&person_dir, "alpha-skill");
    fs::write(person_dir.join(INSTALLED_FROM_MARKER), "local\n").unwrap();

    let agent_dir = write_toml_plugin(
        &f.config.workspace_plugins_dir,
        "beta",
        "schema_version = 1\n[plugin]\nname = \"beta\"\nversion = \"2.0.0\"\n[commands]\npath = \"commands\"\n",
    );
    fs::create_dir_all(agent_dir.join("commands")).unwrap();
    fs::write(agent_dir.join("commands/beta-cmd.md"), "do beta\n").unwrap();

    let unreviewed_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "gamma",
        "schema_version = 1\n[plugin]\nname = \"gamma\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    );
    write_skill(&unreviewed_dir, "gamma-skill");

    let builtin_dir = write_toml_plugin(
        &f.config.builtin_plugin_dirs[0],
        "core-pack",
        "schema_version = 1\n[plugin]\nname = \"core-pack\"\nversion = \"0.1.0\"\n[skills]\npath = \"skills\"\n",
    );
    write_skill(&builtin_dir, "builtin-skill");

    let mut registry = discover_with_config(&f.config);
    approve_reviewed(
        &mut registry,
        "alpha",
        PluginApprover::person(PersonVia::Terminal),
    );
    registry.enable("alpha").unwrap();

    let agent_approver = PluginApprover::Agent(AgentApproval {
        posture: AgentPosture::AutoReview,
        workspace: f.config.workspace.to_string_lossy().into_owned(),
        thread_id: Some("thread-alpha".into()),
        turn_id: Some("turn-alpha".into()),
        tool_call_id: Some("call-alpha".into()),
    });
    approve_reviewed(&mut registry, "beta", agent_approver.clone());
    registry.enable_by("beta", agent_approver).unwrap();

    f.reload_registry();

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["truncated"], false);
    assert!(listed.get("diagnostic").is_none());
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 4);

    let find = |name: &str| {
        entries
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("missing {name}: {listed}"))
    };

    let alpha = find("alpha");
    assert_eq!(alpha["version"], "1.2.3");
    assert_eq!(alpha["description"], "Alpha plugin");
    assert_eq!(alpha["enabled"], true);
    assert_eq!(alpha["installed"], true);
    assert_eq!(alpha["removable"], true);
    assert!(alpha.get("readOnlyReason").is_none());
    assert_eq!(alpha["availability"], "profile");
    assert_eq!(alpha["official"], false);
    assert_eq!(alpha["optional"], false);
    assert_eq!(alpha["updatable"], false);
    assert_eq!(alpha["overrides"], json!([]));
    assert_eq!(
        alpha["rows"],
        json!([{
            "rowId": "skills",
            "moduleName": "alpha/skills",
            "entryId": "alpha/skills",
        }])
    );
    assert_eq!(alpha["codewhale"]["plugin"], "alpha");
    assert_eq!(alpha["codewhale"]["scope"], "user");
    assert_eq!(alpha["codewhale"]["trust"], "trusted");
    assert_eq!(alpha["codewhale"]["active"], true);
    assert_eq!(alpha["codewhale"]["approved_by"]["kind"], "person");
    assert_eq!(alpha["codewhale"]["approved_by"]["via"], "terminal");
    assert_eq!(alpha["codewhale"]["approved_by"]["this_project"], true);
    let services = alpha["codewhale"]["services"].as_array().unwrap();
    assert_eq!(services.len(), 2, "{services:?}");
    assert!(services.contains(&json!({"service": "network", "state": "can use"})));
    assert!(services.contains(&json!({
        "service": "llm",
        "state": "this version does not offer it",
    })));

    let beta = find("beta");
    assert_eq!(beta["removable"], false);
    assert_eq!(beta["availability"], "profile");
    assert_eq!(beta["codewhale"]["scope"], "workspace");
    assert_eq!(beta["codewhale"]["approved_by"]["kind"], "agent");
    assert_eq!(beta["codewhale"]["approved_by"]["posture"], "auto_review");
    assert_eq!(beta["codewhale"]["approved_by"]["this_project"], true);
    assert_eq!(beta["codewhale"]["enabled_by"]["kind"], "agent");
    assert_eq!(beta["codewhale"]["enabled_by"]["posture"], "auto_review");
    assert_eq!(beta["codewhale"]["enabled_by"]["this_project"], true);

    let gamma = find("gamma");
    assert_eq!(gamma["enabled"], false);
    assert_eq!(gamma["removable"], false);
    assert_eq!(gamma["codewhale"]["trust"], "not-reviewed");
    assert_eq!(gamma["codewhale"]["approved_by"], json!(null));
    assert_eq!(gamma["codewhale"]["enabled_by"], json!(null));

    let builtin = find("core-pack");
    assert_eq!(builtin["availability"], "installation");
    assert_eq!(builtin["removable"], false);
    assert_eq!(builtin["readOnlyReason"], "management-required");
    assert_eq!(builtin["codewhale"]["scope"], "builtin");

    let status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    let status_rows = status["entries"].as_array().unwrap();
    assert_eq!(status_rows.len(), 4);
    let alpha_row = status_rows
        .iter()
        .find(|row| row["entryId"] == "alpha/skills")
        .unwrap();
    assert_eq!(alpha_row["patchId"], "alpha");
    assert!(alpha_row.get("readOnlyReason").is_none());
    let builtin_row = status_rows
        .iter()
        .find(|row| row["entryId"] == "core-pack/skills")
        .unwrap();
    assert_eq!(builtin_row["readOnlyReason"], "management-required");
    assert!(builtin_row.get("patchId").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn an_agent_record_of_another_project_shows_no_path_and_no_conversation() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();

    let other_workspace = f._temp.path().join("secret-other-project-workspace-77");
    fs::create_dir_all(&other_workspace).unwrap();
    let plugin_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "shared-agent",
        "schema_version = 1\n[plugin]\nname = \"shared-agent\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    );
    write_skill(&plugin_dir, "other-skill");

    let other_config = DiscoveryConfig {
        workspace: other_workspace.clone(),
        ..f.config.clone()
    };
    let mut other_registry = discover_with_config(&other_config);
    let foreign_thread = "thread-secret-9988";
    let foreign_turn = "turn-secret-7766";
    let foreign_call = "call-secret-5544";
    let foreign_approver = PluginApprover::Agent(AgentApproval {
        posture: AgentPosture::FullAccess,
        workspace: other_workspace.to_string_lossy().into_owned(),
        thread_id: Some(foreign_thread.into()),
        turn_id: Some(foreign_turn.into()),
        tool_call_id: Some(foreign_call.into()),
    });
    approve_reviewed(
        &mut other_registry,
        "shared-agent",
        foreign_approver.clone(),
    );
    other_registry
        .enable_by("shared-agent", foreign_approver)
        .unwrap();

    f.reload_registry();

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let wire = listed.to_string();
    assert!(
        !wire.contains("secret-other-project-workspace-77"),
        "{wire}"
    );
    assert!(!wire.contains(foreign_thread), "{wire}");
    assert!(!wire.contains(foreign_turn), "{wire}");
    assert!(!wire.contains(foreign_call), "{wire}");

    let entry = &listed["entries"][0];
    assert_eq!(entry["codewhale"]["approved_by"]["kind"], "agent");
    assert_eq!(entry["codewhale"]["approved_by"]["posture"], "full_access");
    assert_eq!(entry["codewhale"]["approved_by"]["this_project"], false);
    assert!(entry["codewhale"]["approved_by"].get("workspace").is_none());
    assert!(entry["codewhale"]["approved_by"].get("thread_id").is_none());
    assert!(entry["codewhale"]["approved_by"].get("turn_id").is_none());
    assert!(
        entry["codewhale"]["approved_by"]
            .get("tool_call_id")
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn status_carries_the_hosts_activation_error() {
    let mut f = PluginsFixture::new();
    let _manager = TestManagerGuard::install(Arc::clone(&f.manager));
    let _policy = TestPolicyGuard::extension_host(true);

    let native_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "native-broken",
        "schema_version = 1\n[plugin]\nname = \"native-broken\"\nversion = \"0.2.0\"\n[native]\npath = \"index.mjs\"\n[skills]\npath = \"skills\"\n",
    );
    fs::write(native_dir.join("index.mjs"), "export function apply() {}\n").unwrap();
    write_skill(&native_dir, "helper-skill");

    let mut registry = discover_with_config(&f.config);
    approve_reviewed(
        &mut registry,
        "native-broken",
        PluginApprover::person(PersonVia::Terminal),
    );
    registry.enable("native-broken").unwrap();
    let loaded_registry = f.reload_registry();
    let plugin_id = loaded_registry
        .get("native-broken")
        .unwrap()
        .id
        .as_str()
        .to_string();

    {
        let mut owners = f.manager.shared.registry.lock().unwrap();
        let owner = owners
            .begin_owner(
                HostCohort::reviewed(),
                &plugin_id,
                "native-broken",
                Some(fake_authority(&plugin_id)),
                "hash",
            )
            .unwrap();
        owners.mark_failed(&owner, OwnerState::Failed("boom initializing host".into()));
    }

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let status_on = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    let rows_on = status_on["entries"].as_array().unwrap();
    assert_eq!(rows_on.len(), 2);
    let native_on = rows_on
        .iter()
        .find(|row| row["entryId"] == "native-broken/native/index.mjs")
        .unwrap();
    let skill_on = rows_on
        .iter()
        .find(|row| row["entryId"] == "native-broken/skills")
        .unwrap();
    assert_eq!(native_on["enabled"], true);
    assert_eq!(native_on["fiberPhase"], "failed");
    assert_eq!(native_on["readOnlyReason"], "unaddressable");
    assert!(native_on.get("patchId").is_none());
    assert!(
        native_on["diagnostic"]
            .as_str()
            .unwrap()
            .contains("boom initializing host"),
        "{native_on}"
    );
    assert_eq!(skill_on["fiberPhase"], json!(null));
    assert_eq!(skill_on["readOnlyReason"], "unaddressable");

    drop(_policy);
    let _policy_off = TestPolicyGuard::extension_host(false);
    let status_off = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    let native_off = status_off["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["entryId"] == "native-broken/native/index.mjs")
        .unwrap();
    assert_eq!(native_off["fiberPhase"], json!(null));
    assert!(
        native_off["diagnostic"]
            .as_str()
            .unwrap()
            .contains("extension host is switched off"),
        "{native_off}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn posture_names_the_live_mode_in_dsh_words() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let cases = [
        (AppMode::Plan, ApprovalMode::Suggest, "read-only", "plan"),
        (AppMode::Plan, ApprovalMode::Bypass, "read-only", "plan"),
        (
            AppMode::Agent,
            ApprovalMode::Suggest,
            "workspace-write",
            "ask",
        ),
        (
            AppMode::Agent,
            ApprovalMode::AcceptEdits,
            "workspace-write",
            "accept_edits",
        ),
        (
            AppMode::Agent,
            ApprovalMode::Auto,
            "workspace-write",
            "auto_review",
        ),
        (
            AppMode::Agent,
            ApprovalMode::Bypass,
            "danger-full-access",
            "full_access",
        ),
        (
            AppMode::Agent,
            ApprovalMode::Never,
            "workspace-write",
            "never",
        ),
    ];

    for (app_mode, approval_mode, expected_mode, expected_permission) in cases {
        f.posture.switch_mode_for_tests(app_mode, approval_mode);
        let answer = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "posture",
                json!({}),
            )
            .await
            .unwrap();
        assert_eq!(
            answer,
            json!({
                "mode": expected_mode,
                "permission_mode": expected_permission,
                "admitted_by_person": false,
            })
        );
    }
    drop(guard);

    let admitted = f
        .context
        .clone()
        .with_human_decision(HumanDecision::for_test("creator_tool", &json!({})));
    f.posture
        .switch_mode_for_tests(AppMode::Agent, ApprovalMode::Suggest);
    let admitted_guard = f.begin(&f.creator, HostCohort::Builtin, &admitted);
    let admitted_ticket = plugin_ticket(&admitted_guard).unwrap();
    let answer = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &admitted_ticket,
            "posture",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(
        answer,
        json!({
            "mode": "workspace-write",
            "permission_mode": "ask",
            "admitted_by_person": true,
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_raises_no_card_in_ask_and_lands_on_the_tool_result() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture
        .switch_mode_for_tests(AppMode::Agent, ApprovalMode::Suggest);
    let card_prompts = Arc::new(AtomicUsize::new(0));
    let card_prompts_in_gate = Arc::clone(&card_prompts);
    f.gate = NestedCallGate::answering_for_test(move |_, _| {
        card_prompts_in_gate.fetch_add(1, Ordering::SeqCst);
        NestedCallVerdict::Refused {
            error: ToolError::permission_denied("unexpected card"),
            decision: NestedDecision::Denied,
        }
    })
    .for_extension(
        ExtensionCaller {
            origin: "extension:creator".to_string(),
            tool: "creator_tool".to_string(),
            scope: "ext:host:creator@digest".to_string(),
        },
        BrokerActTool::all().to_vec(),
    );

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for method in ["list", "status", "posture"] {
        let answer = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, json!({}))
            .await;
        assert!(answer.is_ok(), "{method}: {answer:?}");
    }
    assert_eq!(f.asked.load(Ordering::SeqCst), 0);
    assert_eq!(card_prompts.load(Ordering::SeqCst), 0);

    let receipts = guard.receipts().expect("receipts");
    assert_eq!(receipts["broker_acts"]["total"], 3, "{receipts}");
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(acts.len(), 3);
    for (act, method) in acts.iter().zip(["list", "status", "posture"]) {
        assert_eq!(act["service"], "plugins");
        assert_eq!(act["method"], method);
        assert_eq!(act["outcome"], "read");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_wrong_shaped_input_gets_invalid_input() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for method in ["list", "status", "posture"] {
        for bad_input in [
            json!({"workspace": "/elsewhere"}),
            json!({"session": "other"}),
            json!({"offset": 1}),
            json!([1]),
            json!("bad"),
        ] {
            let err = f
                .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, bad_input)
                .await;
            assert_eq!(
                refusal(&err),
                (error_code::INVALID_PARAMS, "invalid_input".into()),
                "{method}"
            );
        }
    }
    assert!(guard.receipts().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_sub_agent_caller_is_refused() {
    let mut f = PluginsFixture::new();
    f.reload_registry();

    let mut sub_context = f.context.clone();
    sub_context.execution.owner_agent_id = Some("sub-agent-1".into());
    let sub_guard = f.begin(&f.creator, HostCohort::Builtin, &sub_context);
    assert!(plugin_ticket(&sub_guard).is_none());

    let root_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let root_ticket = plugin_ticket(&root_guard).unwrap();
    drop(root_guard);

    for method in ["list", "status", "posture"] {
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &root_ticket,
                method,
                json!({}),
            )
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::REFUSED, "ticket_refused".into()),
            "{method}"
        );
    }
    assert!(sub_guard.receipts().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn an_owner_that_is_not_the_creator_module_is_refused() {
    let mut f = PluginsFixture::new();
    f.reload_registry();

    {
        let mut owners = f.manager.shared.registry.lock().unwrap();
        let forged_err = owners
            .begin_owner(
                HostCohort::reviewed(),
                "host:creator",
                "forged-creator",
                Some(fake_authority("forged-creator")),
                "hash",
            )
            .unwrap_err();
        assert!(forged_err.contains("host:creator"), "{forged_err}");
    }

    let plugin_guard = f.begin(&f.plugin_owner, HostCohort::reviewed(), &f.context);
    assert!(plugin_ticket(&plugin_guard).is_none());
    let mcp_guard = f.begin(&f.mcp, HostCohort::Builtin, &f.context);
    assert!(plugin_ticket(&mcp_guard).is_none());

    let forged_owner = OwnerRef {
        plugin_id: "host:creator".into(),
        generation: 99,
        owner_token: "forged-token".into(),
    };

    let creator_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let creator_ticket = plugin_ticket(&creator_guard).unwrap();

    for method in ["list", "status", "posture"] {
        for (owner, tier) in [
            (&f.plugin_owner, HostCohort::reviewed()),
            (&f.mcp, HostCohort::Builtin),
            (&forged_owner, HostCohort::reviewed()),
        ] {
            let err = f
                .call_broker(owner, tier, &creator_ticket, method, json!({}))
                .await;
            assert_eq!(
                refusal(&err),
                (error_code::REFUSED, "ticket_refused".into()),
                "{owner:?} {method}"
            );
        }

        for owner in [&f.plugin_owner, &f.mcp] {
            let arm_err = serve(detached_call(owner.clone(), method, json!({}))).await;
            assert!(
                matches!(
                    arm_err,
                    Err(super::super::dsh_broker::BrokerRefusal::Refused {
                        service: DshService::Plugins,
                        ..
                    })
                ),
                "{owner:?} {method}: {arm_err:?}"
            );
        }
    }
    assert!(creator_guard.receipts().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_session_without_a_registry_answers_not_available() {
    let f = PluginsFixture::new();
    assert!(f.context.plugin_registry.is_none());
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for method in ["list", "status"] {
        let err = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, json!({}))
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::NOT_AVAILABLE, "not_available".into()),
            "{method}"
        );
    }

    let mut no_posture = f.context.clone();
    no_posture.live_posture = None;
    no_posture.plugin_registry = Some(Arc::new(discover_with_config(&f.config)));
    let no_posture_guard = f.begin(&f.creator, HostCohort::Builtin, &no_posture);
    let no_posture_ticket = plugin_ticket(&no_posture_guard).unwrap();
    let err = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &no_posture_ticket,
            "posture",
            json!({}),
        )
        .await;
    assert_eq!(
        refusal(&err),
        (error_code::NOT_AVAILABLE, "not_available".into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_long_list_is_cut_and_says_so() {
    let mut f = PluginsFixture::new();
    let skill_paths: Vec<String> = (0..35).map(|idx| format!("skills/s{idx:02}")).collect();
    let command_paths: Vec<String> = (0..35).map(|idx| format!("commands/c{idx:02}")).collect();
    let big_manifest = format!(
        "schema_version = 1\n[plugin]\nname = \"many-rows\"\nversion = \"1.0.0\"\n[skills]\npaths = {skill_paths:?}\n[commands]\npaths = {command_paths:?}\n"
    );
    let big_dir = write_toml_plugin(&f.config.user_plugins_dir, "many-rows", &big_manifest);
    for rel in skill_paths.iter().chain(command_paths.iter()) {
        fs::create_dir_all(big_dir.join(rel)).unwrap();
    }

    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let single_listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    assert_eq!(single_listed["truncated"], true);
    assert_eq!(
        single_listed["entries"][0]["rows"]
            .as_array()
            .unwrap()
            .len(),
        64
    );

    let single_status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(single_status["truncated"], false);
    assert_eq!(single_status["entries"].as_array().unwrap().len(), 70);

    for idx in 0..105 {
        let name = format!("pack-{idx:03}");
        let dir = write_toml_plugin(
            &f.config.user_plugins_dir,
            &name,
            &format!(
                "schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n"
            ),
        );
        write_skill(&dir, "s1");
    }

    drop(guard);
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["truncated"], true);
    assert_eq!(listed["entries"].as_array().unwrap().len(), 100);

    let status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(status["truncated"], true);
    assert_eq!(status["entries"].as_array().unwrap().len(), 100);
}

#[tokio::test(flavor = "current_thread")]
async fn changing_methods_reject_unknown_fields_and_missing_registry_or_posture() {
    let mut f = PluginsFixture::new();
    let no_registry_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let no_registry_ticket = plugin_ticket(&no_registry_guard).unwrap();
    for (method, input) in [
        ("install", json!({"target": "alpha"})),
        ("remove", json!({"target": "alpha"})),
        ("enable", json!({"target": "alpha"})),
        ("disable", json!({"target": "alpha"})),
    ] {
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &no_registry_ticket,
                method,
                input,
            )
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::NOT_AVAILABLE, "not_available".into()),
            "missing registry {method}"
        );
    }
    drop(no_registry_guard);

    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for (method, input) in [
        ("install", json!({"target": "alpha", "extra": 1})),
        ("remove", json!({"target": "alpha", "extra": 1})),
        ("enable", json!({"target": "alpha", "extra": 1})),
        ("disable", json!({"target": "alpha", "extra": 1})),
        ("install", json!({})),
        ("remove", json!({})),
        ("enable", json!({})),
        ("disable", json!({})),
    ] {
        let err = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, input)
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::INVALID_PARAMS, "invalid_input".into()),
            "schema {method}"
        );
    }

    let mut no_posture = f.context.clone();
    no_posture.live_posture = None;
    let np_guard = f.begin(&f.creator, HostCohort::Builtin, &no_posture);
    let np_ticket = plugin_ticket(&np_guard).unwrap();
    for method in ["install", "remove", "enable", "disable"] {
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &np_ticket,
                method,
                json!({"target": "alpha"}),
            )
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::NOT_AVAILABLE, "not_available".into()),
            "no posture {method}"
        );
    }
    assert!(guard.receipts().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_state_error_lists_plugins_as_disabled_with_scrubbed_top_diagnostic() {
    let mut f = PluginsFixture::new();
    let dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "corrupt-state-plugin",
        "schema_version = 1\n[plugin]\nname = \"corrupt-state-plugin\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    );
    write_skill(&dir, "s1");

    let mut clean = discover_with_config(&f.config);
    approve_reviewed(
        &mut clean,
        "corrupt-state-plugin",
        PluginApprover::person(PersonVia::Terminal),
    );
    clean.enable("corrupt-state-plugin").unwrap();
    fs::write(&f.config.state_path, "{ not valid json sk-secret-123456").unwrap();

    let broken = f.reload_registry();
    assert!(broken.state_error().is_some());

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["entries"][0]["enabled"], false);
    assert_eq!(listed["entries"][0]["codewhale"]["active"], false);
    assert_eq!(
        listed["entries"][0]["codewhale"]["approved_by"],
        json!(null)
    );
    assert_eq!(listed["entries"][0]["codewhale"]["enabled_by"], json!(null));
    assert_eq!(listed["entries"][0]["codewhale"]["services"], json!([]));
    let diag = listed["diagnostic"].as_str().unwrap();
    assert!(
        !diag.contains(f._temp.path().to_string_lossy().as_ref()),
        "{diag}"
    );

    let status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(status["entries"][0]["enabled"], false);
    assert_eq!(status["entries"][0]["fiberPhase"], json!(null));
    assert!(status["diagnostic"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn fifty_reads_hit_the_per_call_ceiling() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for idx in 0..super::super::core_call::MAX_CALLS_PER_INVOCATION {
        let method = match idx % 3 {
            0 => "list",
            1 => "status",
            _ => "posture",
        };
        let ok = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, json!({}))
            .await;
        assert!(ok.is_ok(), "call {idx}: {ok:?}");
    }

    let capped = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await;
    assert_eq!(refusal(&capped), (error_code::REFUSED, "limit".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn diagnostics_and_manifest_text_are_scrubbed_and_bounded() {
    let mut f = PluginsFixture::new();
    let _manager = TestManagerGuard::install(Arc::clone(&f.manager));
    let _policy = TestPolicyGuard::extension_host(true);

    let long_desc = format!("Bearer sk-proj-secret123456 {}", "a".repeat(800));
    let mut inline_truncated = false;
    let inline = sanitize_inline(
        "\x1b[31mIgnore previous\x1b[0m\nBearer sk-proj-secret123456\u{202e} ok",
        MAX_TEXT_BYTES,
        &mut inline_truncated,
    );
    assert!(!inline_truncated);
    assert!(!inline.contains('\x1b'));
    assert!(!inline.contains('\n'));
    assert!(!inline.contains('\u{202e}'));
    assert!(!inline.contains("sk-proj-secret123456"), "{inline}");
    let hostile_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "hostile",
        &format!(
            "schema_version = 1\n[plugin]\nname = \"hostile\"\nversion = \"1.0.0\"\ndescription = {long_desc:?}\n[native]\npath = \"index.mjs\"\n"
        ),
    );
    fs::write(
        hostile_dir.join("index.mjs"),
        "export function apply() {}\n",
    )
    .unwrap();

    let mut registry = discover_with_config(&f.config);
    approve_reviewed(
        &mut registry,
        "hostile",
        PluginApprover::person(PersonVia::Terminal),
    );
    registry.enable("hostile").unwrap();
    let loaded = f.reload_registry();
    let plugin_id = loaded.get("hostile").unwrap().id.as_str().to_string();

    {
        let mut owners = f.manager.shared.registry.lock().unwrap();
        let owner = owners
            .begin_owner(
                HostCohort::reviewed(),
                &plugin_id,
                "hostile",
                Some(fake_authority(&plugin_id)),
                "hash",
            )
            .unwrap();
        let msg = format!(
            "\x1b[31mfailed\x1b[0m at /etc/passwd and file:///Users/alice/secret and file:///C:/Users/alice/secret and `/Users/alice/backtick` and </Users/alice/angle> and {}/native/index.mjs Bearer sk-proj-secret123456 \u{202e}https://example.com/docs/a {}",
            loaded.get("hostile").unwrap().canonical_root.display(),
            "z".repeat(1200)
        );
        owners.mark_failed(&owner, OwnerState::Failed(msg));
    }

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["truncated"], true);
    let desc = listed["entries"][0]["description"].as_str().unwrap();
    assert!(desc.len() <= 512, "{desc}");
    assert!(!desc.contains('\x1b'));
    assert!(!desc.contains('\n'));
    assert!(!desc.contains('\u{202e}'));
    assert!(!desc.contains("sk-proj-secret123456"), "{desc}");

    let status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(status["truncated"], true);
    let diag = status["entries"][0]["diagnostic"].as_str().unwrap();
    assert!(diag.len() <= 1024, "{diag}");
    assert!(!diag.contains("/etc/passwd"), "{diag}");
    assert!(!diag.contains("file://"), "{diag}");
    assert!(!diag.contains("/Users/alice"), "{diag}");
    assert!(!diag.contains("C:/Users/alice"), "{diag}");
    assert!(!diag.contains("backtick"), "{diag}");
    assert!(!diag.contains("angle"), "{diag}");
    assert!(diag.contains("<outside-path>"), "{diag}");
    assert!(!diag.contains("sk-proj-secret123456"), "{diag}");
    assert!(!diag.contains('\x1b'));
    assert!(!diag.contains('\u{202e}'));
    assert!(diag.contains("https://example.com/docs/a"), "{diag}");
    assert!(diag.contains("native/index.mjs"), "{diag}");

    let extra = clean_diagnostic_fragment(
        "bad={/var/secret} and|<file:///C:/Users/alice/win> and|C:\\Users\\alice\\win2",
    );
    assert!(!extra.contains("/var/secret"), "{extra}");
    assert!(!extra.contains("file://"), "{extra}");
    assert!(!extra.contains("Users"), "{extra}");
    assert!(extra.contains("<outside-path>"), "{extra}");
    assert_eq!(
        diagnostic_path(
            Path::new("/private/var/tmp/p"),
            Path::new("/var/tmp/p/plugin.toml")
        ),
        "plugin.toml"
    );
    assert_eq!(
        diagnostic_path(
            Path::new("/private/var/tmp/p"),
            Path::new("/private/var/tmp/p/../../etc/passwd")
        ),
        "<outside-path>"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn symlinked_install_record_does_not_mark_user_plugin_removable() {
    let mut f = PluginsFixture::new();
    let plugin_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "sym-user",
        "schema_version = 1\n[plugin]\nname = \"sym-user\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    );
    write_skill(&plugin_dir, "s1");
    f.reload_registry();
    let target = f._temp.path().join("outside-marker");
    fs::write(&target, "local\n").unwrap();
    std::os::unix::fs::symlink(&target, plugin_dir.join(INSTALLED_FROM_MARKER)).unwrap();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["entries"].as_array().unwrap().len(), 1);
    assert_eq!(listed["entries"][0]["name"], "sym-user");
    assert_eq!(listed["entries"][0]["removable"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn status_keeps_later_plugins_when_an_earlier_plugin_truncates_a_field() {
    let mut f = PluginsFixture::new();
    let _manager = TestManagerGuard::install(Arc::clone(&f.manager));
    let _policy = TestPolicyGuard::extension_host(true);

    let first_dir = write_toml_plugin(
        &f.config.user_plugins_dir,
        "00-hostile",
        "schema_version = 1\n[plugin]\nname = \"00-hostile\"\nversion = \"1.0.0\"\n[native]\npath = \"index.mjs\"\n",
    );
    fs::write(first_dir.join("index.mjs"), "export function apply() {}\n").unwrap();

    let second_dir = write_toml_plugin(
        &f.config.workspace_plugins_dir,
        "zz-healthy",
        "schema_version = 1\n[plugin]\nname = \"zz-healthy\"\nversion = \"1.0.0\"\n[native]\npath = \"index.mjs\"\n",
    );
    fs::write(second_dir.join("index.mjs"), "export function apply() {}\n").unwrap();

    let mut registry = discover_with_config(&f.config);
    for name in ["00-hostile", "zz-healthy"] {
        approve_reviewed(
            &mut registry,
            name,
            PluginApprover::person(PersonVia::Terminal),
        );
        registry.enable(name).unwrap();
    }
    let loaded = f.reload_registry();
    let first_id = loaded.get("00-hostile").unwrap().id.as_str().to_string();
    let second_id = loaded.get("zz-healthy").unwrap().id.as_str().to_string();

    {
        let mut owners = f.manager.shared.registry.lock().unwrap();
        let first_owner = owners
            .begin_owner(
                HostCohort::reviewed(),
                &first_id,
                "00-hostile",
                Some(fake_authority(&first_id)),
                "hash-1",
            )
            .unwrap();
        owners.mark_failed(&first_owner, OwnerState::Failed("boom ".repeat(300)));

        let second_owner = owners
            .begin_owner(
                HostCohort::reviewed(),
                &second_id,
                "zz-healthy",
                Some(fake_authority(&second_id)),
                "hash-2",
            )
            .unwrap();
        owners.mark_active(&second_owner);
    }
    f.manager.shared.plugin_diagnostic(
        &second_id,
        "extension `zz-healthy` active (tools: ping; commands: )".into(),
    );

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(status["truncated"], true);
    let rows = status["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{status}");
    assert_eq!(rows[0]["entryId"], "00-hostile/native/index.mjs");
    assert_eq!(rows[0]["fiberPhase"], "failed");
    assert!(rows[0]["diagnostic"].as_str().unwrap().len() <= 1024);
    assert_eq!(rows[1]["entryId"], "zz-healthy/native/index.mjs");
    assert_eq!(rows[1]["fiberPhase"], "active");
    assert!(rows[1].get("diagnostic").is_none(), "{rows:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn plugins_tools_returns_the_invocation_caller_tools() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    let specs = crate::tools::registry::ToolRegistryBuilder::new()
        .with_file_tools()
        .build(ToolContext::new(f.context.workspace.clone()))
        .all();
    let caller = f.gate.extension().unwrap().0.clone();
    f.gate = f.gate.clone().for_extension(caller, specs);

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let answer = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "tools", json!({}))
        .await
        .unwrap();
    let names = answer["names"].as_array().unwrap();
    assert!(names.iter().any(|name| name.as_str() == Some("read_file")));
    assert!(names.iter().any(|name| name.as_str() == Some("write_file")));
}

#[tokio::test(flavor = "current_thread")]
async fn plugins_tools_leaves_out_hidden_and_disallowed_tools() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    let mut specs = crate::tools::registry::ToolRegistryBuilder::new()
        .with_file_tools()
        .build(ToolContext::new(f.context.workspace.clone()))
        .all();
    specs.extend(BrokerActTool::all().to_vec());
    let caller = f.gate.extension().unwrap().0.clone();
    f.gate = f.gate.clone().for_extension(caller, specs);
    f.context.disallowed_tools = vec!["read_file".to_string()];

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let answer = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "tools", json!({}))
        .await
        .unwrap();
    let names = answer["names"].as_array().unwrap();
    assert!(!names.iter().any(|name| name.as_str() == Some("read_file")));
    assert!(
        !names
            .iter()
            .any(|name| name.as_str() == Some("dsh_broker_act"))
    );
    assert!(
        !names
            .iter()
            .any(|name| name.as_str() == Some("dsh_broker_reach"))
    );
    assert!(names.iter().any(|name| name.as_str() == Some("write_file")));
}

#[tokio::test(flavor = "current_thread")]
async fn plugins_tools_is_refused_for_a_plugin_tier_owner_and_a_sub_agent() {
    let _policy = TestPolicyGuard::extension_host(true);
    let f = PluginsFixture::new();

    let guard = f.begin(&f.plugin_owner, HostCohort::reviewed(), &f.context);
    assert!(plugin_ticket(&guard).is_none());

    let mut sub_context = f.context.clone();
    sub_context.execution.owner_agent_id = Some("sub-agent-1".into());
    let sub_guard = f.begin(&f.creator, HostCohort::Builtin, &sub_context);
    assert!(plugin_ticket(&sub_guard).is_none());

    let creator_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let creator_ticket = plugin_ticket(&creator_guard).unwrap();
    let plugin_err = f
        .call_broker(
            &f.plugin_owner,
            HostCohort::reviewed(),
            &creator_ticket,
            "tools",
            json!({}),
        )
        .await;
    assert_eq!(
        refusal(&plugin_err),
        (error_code::REFUSED, "ticket_refused".into())
    );
    let arm_err = serve(detached_call(f.plugin_owner.clone(), "tools", json!({}))).await;
    assert!(matches!(
        arm_err,
        Err(super::super::dsh_broker::BrokerRefusal::Refused {
            service: DshService::Plugins,
            ..
        })
    ));
    drop(creator_guard);
    let revoked_err = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &creator_ticket,
            "tools",
            json!({}),
        )
        .await;
    assert_eq!(
        refusal(&revoked_err),
        (error_code::REFUSED, "ticket_refused".into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plugins_list_and_status_never_return_config_values() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    let secret = "SECRET_VALUE_NEVER_LEAK_98765";
    let body = format!(
        "schema_version = 1\n[plugin]\nname = \"secretive\"\nversion = \"1.0.0\"\n[plugins.secretive.config]\napi_key = \"{secret}\"\n"
    );
    write_toml_plugin(&f.config.user_plugins_dir, "secretive", &body);
    f.reload_registry();

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let list = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let status = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "status",
            json!({}),
        )
        .await
        .unwrap();
    let list_str = serde_json::to_string(&list).unwrap();
    let status_str = serde_json::to_string(&status).unwrap();
    assert!(!list_str.contains(secret));
    assert!(!status_str.contains(secret));
}

fn copy_dir_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

type GateCallLog = Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

fn install_gate(
    f: &mut PluginsFixture,
    on_call: impl Fn(&Value, &LivePosture) -> NestedCallVerdict + Send + Sync + 'static,
) -> GateCallLog {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_ref = Arc::clone(&seen);
    let asked_ref = Arc::clone(&f.asked);
    let posture_ref = f.posture.clone();
    f.gate = NestedCallGate::answering_for_test(move |name, input| {
        asked_ref.fetch_add(1, Ordering::SeqCst);
        let target = input
            .get("target")
            .and_then(Value::as_str)
            .map(str::to_string);
        seen_ref.lock().unwrap().push((name.to_string(), target));
        let mut verdict = on_call(input, &posture_ref);
        if let NestedCallVerdict::Run {
            name: ref mut verdict_name,
            ..
        } = verdict
            && verdict_name.is_empty()
        {
            *verdict_name = name.to_string();
        }
        verdict
    })
    .for_extension(
        ExtensionCaller {
            origin: "extension:creator".to_string(),
            tool: "creator_tool".to_string(),
            scope: "ext:host:creator@digest".to_string(),
        },
        BrokerActTool::all().to_vec(),
    );
    seen
}

fn allow_verdict(input: &Value, decision: NestedDecision) -> NestedCallVerdict {
    NestedCallVerdict::Run {
        name: String::new(),
        input: input.clone(),
        supports_parallel: false,
        decision,
        hook_context: None,
    }
}

fn deny_verdict(reason: &str) -> NestedCallVerdict {
    NestedCallVerdict::Refused {
        error: ToolError::permission_denied(reason),
        decision: NestedDecision::Denied,
    }
}

fn write_skill_bundle(root: &Path, dir_name: &str, plugin_name: &str, version: &str) -> PathBuf {
    let body = format!(
        "schema_version = 1\n[plugin]\nname = \"{plugin_name}\"\nversion = \"{version}\"\n"
    );
    let dir = write_toml_plugin(root, dir_name, &body);
    write_skill(&dir, "helper");
    dir
}

fn write_native_bundle(root: &Path, dir_name: &str, plugin_name: &str, version: &str) -> PathBuf {
    let body = format!(
        "schema_version = 1\n[plugin]\nname = \"{plugin_name}\"\nversion = \"{version}\"\n[native]\npath = \"native/index.mjs\"\n"
    );
    let dir = write_toml_plugin(root, dir_name, &body);
    fs::create_dir_all(dir.join("native")).unwrap();
    fs::write(dir.join("native/index.mjs"), "export function apply() {}\n").unwrap();
    dir
}

fn read_install_receipt(plugin_dir: &Path) -> Value {
    let raw = fs::read_to_string(plugin_dir.join(INSTALLED_FROM_MARKER)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn dsh_template_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/assets/dsh-creator/skills/cordis-plugin-development/templates")
        .join(name)
}

#[tokio::test(flavor = "current_thread")]
async fn full_access_installs_a_folder_in_the_project_and_records_the_agent() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    let _manager = TestManagerGuard::install(Arc::new(ExtensionHostManager::new(
        ExtensionHostOptions::default(),
    )));
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    let gate_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let src = write_skill_bundle(
        &f.config.workspace,
        "agent-skill-src",
        "agent-skill",
        "1.2.3",
    );
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let installed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(installed["application"], "applied", "{installed}");
    assert_eq!(installed["stage"], "enable");
    assert_eq!(installed["changed"], true);
    assert_eq!(installed["enabled"], true);
    assert_eq!(installed["bundle"], "agent-skill");
    assert_eq!(installed["version"], "1.2.3");
    let warnings = installed["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w == "enabled for this project only"),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w == "approved by the agent in Full Access"),
        "{warnings:?}"
    );
    assert_eq!(f.asked.load(Ordering::SeqCst), 1);
    assert_eq!(gate_calls.lock().unwrap().len(), 1);

    drop(guard);
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let entry = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "agent-skill")
        .unwrap();
    assert_eq!(entry["codewhale"]["approved_by"]["kind"], "agent");
    assert_eq!(entry["codewhale"]["approved_by"]["posture"], "full_access");
    assert_eq!(entry["codewhale"]["approved_by"]["this_project"], true);
    assert_eq!(entry["codewhale"]["enabled_by"]["this_project"], true);

    let receipt = read_install_receipt(&f.config.user_plugins_dir.join("agent-skill"));
    let recorded_spec = receipt["spec"].as_str().unwrap();
    let recorded_path = recorded_spec.strip_prefix("path:").unwrap_or(recorded_spec);
    assert_eq!(
        fs::canonicalize(recorded_path).unwrap(),
        fs::canonicalize(&src).unwrap(),
        "{receipt}"
    );
    assert!(
        receipt["install_nonce"]
            .as_str()
            .is_some_and(|n| !n.is_empty()),
        "{receipt}"
    );
    assert!(
        receipt["content_digest"]
            .as_str()
            .is_some_and(|h| h.len() == 64),
        "{receipt}"
    );

    let deco_src = f.config.workspace.join("deco-src");
    copy_dir_tree(&dsh_template_dir("decoration"), &deco_src);
    let deco = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": deco_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(deco["application"], "failed", "{deco}");
    assert_eq!(deco["stage"], "enable");
    assert_eq!(deco["changed"], true);
    assert_eq!(deco["enabled"], true);
    assert_eq!(deco["error"]["code"], "operation-error");
    assert_eq!(
        deco["error"]["diagnostic"].as_str(),
        Some("the extension host is not running for this session"),
        "{deco}"
    );

    let mcp_src = f.config.workspace.join("mcp-src");
    copy_dir_tree(&dsh_template_dir("mcp"), &mcp_src);
    let mcp = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": mcp_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(mcp["application"], "failed", "{mcp}");
    assert_eq!(mcp["changed"], false);
    assert_eq!(mcp["error"]["code"], "operation-error");
}

#[tokio::test(flavor = "current_thread")]
async fn full_access_refuses_what_the_agent_did_not_write() {
    let mut f = PluginsFixture::new();
    let ws_user_plugins = f.config.workspace.join("user-plugins");
    f.config.user_plugins_dir = ws_user_plugins.clone();
    let person_dir = write_skill_bundle(&ws_user_plugins, "person-plug", "person-plug", "1.0.0");
    let person_ext = write_skill_bundle(f._temp.path(), "person-ext-src", "person-plug", "1.0.0");
    fs::write(
        person_dir.join(INSTALLED_FROM_MARKER),
        json!({
            "schema_version": 2,
            "spec": format!("path:{}", person_ext.display()),
            "url": null,
            "source_checksum": "",
            "content_digest": "",
            "installed_name": "person-plug",
            "registry_version": null,
            "install_nonce": "nonce-person-refuse",
        })
        .to_string(),
    )
    .unwrap();
    write_skill_bundle(
        &f.config.builtin_plugin_dirs[0],
        "core-pack",
        "core-pack",
        "1.0.0",
    );
    let mut reg = discover_with_config(&f.config);
    approve_reviewed(
        &mut reg,
        "person-plug",
        PluginApprover::person(PersonVia::ApprovalCard),
    );
    let _ = reg;

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let outside = write_skill_bundle(f._temp.path(), "outside-src", "outside-plugin", "1.0.0");
    let git_hidden = write_skill_bundle(
        &f.config.workspace.join(".git"),
        "hidden-plugin",
        "hidden-plugin",
        "1.0.0",
    );
    let inside_user = write_skill_bundle(
        &ws_user_plugins.join("nested"),
        "in-user-src",
        "in-user-plugin",
        "1.0.0",
    );

    let targets: Vec<(&str, String)> = [
        ("npm spec", "npm:pkg@1.0.0".to_string()),
        ("outside workspace", outside.to_string_lossy().into_owned()),
        ("inside .git", git_hidden.to_string_lossy().into_owned()),
        (
            "inside user_plugins_dir",
            inside_user.to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .chain({
        #[cfg(unix)]
        {
            let link_dst = f.config.workspace.join("linked-outside");
            std::os::unix::fs::symlink(outside_path_clone(f._temp.path()), &link_dst).unwrap();
            Some((
                "symlinked workspace dir",
                link_dst.to_string_lossy().into_owned(),
            ))
        }
        #[cfg(not(unix))]
        {
            None
        }
    })
    .collect();

    let state_before = fs::read(&f.config.state_path).unwrap();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for (label, target) in targets {
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": target}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "failed", "{label}: {res}");
        assert_eq!(res["changed"], false, "{label}: {res}");
        assert_eq!(res["error"]["code"], "operation-error", "{label}: {res}");
        assert!(
            res["error"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("ask the person: step to Ask, or use the Plugins page"),
            "{label}: {res}"
        );
    }

    for method in ["disable", "remove"] {
        let person_res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                method,
                json!({"target": "person-plug"}),
            )
            .await
            .unwrap();
        assert_eq!(
            person_res["application"], "failed",
            "{method}: {person_res}"
        );
        assert_eq!(person_res["changed"], false);
        assert!(
            person_res["error"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("was approved by a person"),
            "{person_res}"
        );

        let builtin_res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                method,
                json!({"target": "core-pack"}),
            )
            .await
            .unwrap();
        assert_eq!(
            builtin_res["application"], "failed",
            "{method}: {builtin_res}"
        );
        assert_eq!(builtin_res["changed"], false);
        assert_eq!(builtin_res["error"]["code"], "management-required");
    }

    assert_eq!(f.asked.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read(&f.config.state_path).unwrap(), state_before);
}

#[cfg(unix)]
fn outside_path_clone(temp_root: &Path) -> PathBuf {
    let dir = temp_root.join("outside-symlink-target");
    if !dir.exists() {
        write_skill_bundle(
            temp_root,
            "outside-symlink-target",
            "linked-outside",
            "1.0.0",
        );
    }
    dir
}

#[tokio::test(flavor = "current_thread")]
async fn ask_asks_once_and_records_the_person() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let gate_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });

    let src = write_skill_bundle(f._temp.path(), "ext-ask-src", "ext-ask", "0.9.0");
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "applied", "{res}");
    assert_eq!(res["stage"], "enable");
    assert_eq!(res["changed"], true);
    assert_eq!(res["enabled"], true);
    assert_eq!(f.asked.load(Ordering::SeqCst), 1);
    assert_eq!(gate_calls.lock().unwrap().len(), 1);
    let warnings = res["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w == "enabled for every project"),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w == "approved by you"),
        "{warnings:?}"
    );

    drop(guard);
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let entry = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "ext-ask")
        .unwrap();
    assert_eq!(entry["codewhale"]["approved_by"]["kind"], "person");
    assert_eq!(entry["codewhale"]["approved_by"]["via"], "approval_card");

    let disabled_src = write_skill_bundle(f._temp.path(), "ext-off-src", "ext-off", "0.1.0");
    let res_off = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": disabled_src.to_string_lossy(), "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(res_off["application"], "applied", "{res_off}");
    assert_eq!(res_off["stage"], "install");
    assert_eq!(res_off["changed"], true);
    assert_eq!(res_off["enabled"], false);
    assert_eq!(f.asked.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn a_no_on_the_card_changes_nothing() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(f._temp.path(), "deny-src", "deny-install", "1.0.0");
    let existing = write_skill_bundle(&f.config.workspace, "seeded-src", "seeded-plug", "1.0.0");

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    {
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let init = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": existing.to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(init["application"], "applied");
    }

    let state_before = fs::read(&f.config.state_path).unwrap();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    install_gate(&mut f, |_, _| deny_verdict("denied by user"));
    f.asked.store(0, Ordering::SeqCst);

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    for (method, input) in [
        ("install", json!({"target": src.to_string_lossy()})),
        ("disable", json!({"target": "seeded-plug"})),
        ("enable", json!({"target": "seeded-plug"})),
        ("remove", json!({"target": "seeded-plug"})),
    ] {
        let err = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, input)
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::DENIED, "denied".into()),
            "{method}"
        );
    }
    assert_eq!(f.asked.load(Ordering::SeqCst), 4);
    assert_eq!(fs::read(&f.config.state_path).unwrap(), state_before);
    assert!(!f.config.user_plugins_dir.join("deny-install").exists());
    assert!(f.config.user_plugins_dir.join("seeded-plug").is_dir());
}

#[tokio::test(flavor = "current_thread")]
async fn auto_review_records_the_agent_with_its_posture() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Auto);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let in_ws = write_skill_bundle(&f.config.workspace, "auto-in", "auto-in", "1.0.0");
    let out_ws = write_skill_bundle(f._temp.path(), "auto-out", "auto-out", "1.0.0");
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let ok = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": in_ws.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(ok["application"], "applied", "{ok}");
    assert!(
        ok["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "approved by the agent in Auto-Review"),
        "{ok}"
    );
    assert_eq!(f.asked.load(Ordering::SeqCst), 1);

    drop(guard);
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let entry = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "auto-in")
        .unwrap();
    assert_eq!(entry["codewhale"]["approved_by"]["kind"], "agent");
    assert_eq!(entry["codewhale"]["approved_by"]["posture"], "auto_review");

    let refused = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": out_ws.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(refused["application"], "failed", "{refused}");
    assert_eq!(refused["changed"], false);
    assert_eq!(f.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn plan_changes_nothing() {
    let mut f = PluginsFixture::new();
    let existing = write_skill_bundle(
        &f.config.workspace,
        "plan-existing",
        "plan-existing",
        "1.0.0",
    );
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    {
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": existing.to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "applied");
    }

    let state_before = fs::read(&f.config.state_path).unwrap();
    let new_src = write_skill_bundle(&f.config.workspace, "plan-new", "plan-new", "1.0.0");

    for (label, mode, approval_mode) in [
        ("never", AppMode::Agent, ApprovalMode::Never),
        ("plan", AppMode::Plan, ApprovalMode::Suggest),
    ] {
        f.posture.switch_mode_for_tests(mode, approval_mode);
        let card_prompts = Arc::new(AtomicUsize::new(0));
        let card_prompts_in_gate = Arc::clone(&card_prompts);
        let gate_calls = install_gate(&mut f, move |input, posture| {
            let live = posture.read();
            if live.mode() == AppMode::Plan || live.approval_mode() == ApprovalMode::Never {
                return deny_verdict("blocked by plan or never posture");
            }
            card_prompts_in_gate.fetch_add(1, Ordering::SeqCst);
            allow_verdict(input, NestedDecision::Approved)
        });
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        for (method, input) in [
            ("install", json!({"target": new_src.to_string_lossy()})),
            ("disable", json!({"target": "plan-existing"})),
            ("enable", json!({"target": "plan-existing"})),
            ("remove", json!({"target": "plan-existing"})),
        ] {
            let err = f
                .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, input)
                .await;
            assert_eq!(
                refusal(&err),
                (error_code::DENIED, "denied".into()),
                "{label} {method}"
            );
        }
        assert_eq!(card_prompts.load(Ordering::SeqCst), 0, "{label}");
        assert_eq!(gate_calls.lock().unwrap().len(), 4, "{label}");
    }
    assert_eq!(fs::read(&f.config.state_path).unwrap(), state_before);
    assert!(!f.config.user_plugins_dir.join("plan-new").exists());
    assert!(f.config.user_plugins_dir.join("plan-existing").is_dir());
}

#[tokio::test(flavor = "current_thread")]
async fn the_dial_moved_to_plan_during_the_card_changes_nothing() {
    let mut f = PluginsFixture::new();
    let seeded = write_skill_bundle(&f.config.workspace, "dial-seed", "dial-seed", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    {
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": seeded.to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "applied");
    }

    let state_before = fs::read(&f.config.state_path).unwrap();
    let candidate = write_skill_bundle(f._temp.path(), "dial-cand", "dial-cand", "1.0.0");

    for (method, input) in [
        ("install", json!({"target": candidate.to_string_lossy()})),
        ("disable", json!({"target": "dial-seed"})),
        ("enable", json!({"target": "dial-seed"})),
        ("remove", json!({"target": "dial-seed"})),
    ] {
        f.posture.switch_for_tests(ApprovalMode::Suggest);
        install_gate(&mut f, |input, posture| {
            posture.switch_for_tests(ApprovalMode::Never);
            allow_verdict(input, NestedDecision::Approved)
        });
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let res = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, input)
            .await
            .unwrap();
        assert_eq!(res["application"], "failed", "{method}: {res}");
        assert_eq!(res["changed"], false, "{method}: {res}");
        assert_eq!(res["error"]["code"], "operation-error", "{method}: {res}");
        assert_eq!(fs::read(&f.config.state_path).unwrap(), state_before);
        assert!(!f.config.user_plugins_dir.join("dial-cand").exists());
        assert!(f.config.user_plugins_dir.join("dial-seed").is_dir());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_arm_calls_the_gate_itself() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(f._temp.path(), "gate-src", "gate-plug", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let gate_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for (method, input) in [
        ("install", json!({"target": src.to_string_lossy()})),
        ("disable", json!({"target": "gate-plug"})),
        ("enable", json!({"target": "gate-plug"})),
        ("remove", json!({"target": "gate-plug"})),
    ] {
        let res = f
            .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, input)
            .await
            .unwrap();
        assert_eq!(res["application"], "applied", "{method}: {res}");
    }
    assert_eq!(f.asked.load(Ordering::SeqCst), 4);
    assert_eq!(gate_calls.lock().unwrap().len(), 4);

    let receipts = guard.receipts().expect("receipts");
    assert_eq!(receipts["broker_acts"]["total"], 8, "{receipts}");
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    let gate_acts: Vec<&Value> = acts
        .iter()
        .filter(|act| act.get("effect").is_some())
        .collect();
    let outcome_acts: Vec<&Value> = acts
        .iter()
        .filter(|act| act.get("application").is_some())
        .collect();
    assert_eq!(gate_acts.len(), 4, "{receipts}");
    assert_eq!(outcome_acts.len(), 4, "{receipts}");
    for ((gate_act, outcome_act), method) in gate_acts
        .iter()
        .zip(outcome_acts.iter())
        .zip(["install", "disable", "enable", "remove"])
    {
        assert_eq!(gate_act["method"], method, "{gate_act}");
        assert_eq!(gate_act["outcome"], "allowed", "{gate_act}");
        assert_eq!(outcome_act["method"], method, "{outcome_act}");
        assert_eq!(outcome_act["application"], "applied", "{outcome_act}");
        assert_eq!(outcome_act["approver"], "person", "{outcome_act}");
        assert_eq!(outcome_act["posture"], "approval_card", "{outcome_act}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_install_whose_tree_differs_from_its_receipt_fails() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let src = write_skill_bundle(
        &f.config.workspace,
        "tamper-install-src",
        "tamper-inst",
        "1.0.0",
    );
    super::set_post_install_hook_for_test(Box::new(|installed_dir, _| {
        let _ = fs::write(
            installed_dir.join("skills/helper/SKILL.md"),
            "---\nname: helper\ndescription: tampered\n---\ntampered body\n",
        );
    }));

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "failed", "{res}");
    assert_eq!(res["stage"], "install");
    assert_eq!(res["changed"], true);
    assert_eq!(res["enabled"], false);
    assert_eq!(res["error"]["code"], "operation-error");
    assert_eq!(res["error"]["diagnostic"], "changed after review");

    let reg = discover_with_config(&f.config);
    let loaded = reg.get("tamper-inst").unwrap();
    assert!(!loaded.enabled);
    assert!(!loaded.trusted());
}

#[tokio::test(flavor = "current_thread")]
async fn the_act_names_the_full_hash_before_the_path() {
    use codewhale_plugins::manifest::PluginManifest;

    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(f._temp.path(), "hash-src", "hash-plug", "1.0.0");
    let expected_hash = PluginManifest::validate_from_path(&src.join("plugin.toml"))
        .unwrap()
        .content_hash;
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let gate_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let installed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(installed["application"], "applied");
    let disabled = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "disable",
            json!({"target": "hash-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(disabled["application"], "applied");
    let enabled = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "enable",
            json!({"target": "hash-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(enabled["application"], "applied");

    let reg = discover_with_config(&f.config);
    let loaded = reg.get("hash-plug").unwrap();
    let calls = gate_calls.lock().unwrap();
    assert_eq!(calls.len(), 3);

    let install_target = calls[0].1.as_deref().unwrap();
    let src_str = src.to_string_lossy();
    let expected_install_prefix = format!("hash-plug {expected_hash} {src_str}");
    assert_eq!(install_target, expected_install_prefix);
    assert!(
        install_target.find(&expected_hash).unwrap()
            < install_target.find(src_str.as_ref()).unwrap()
    );

    let enable_target = calls[2].1.as_deref().unwrap();
    assert_eq!(
        enable_target,
        format!("hash-plug {} {}", loaded.content_hash, loaded.id.as_str())
    );
    assert!(
        enable_target.find(&loaded.content_hash).unwrap()
            < enable_target.find(loaded.id.as_str()).unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn bytes_changed_after_the_gate_fail_as_changed_after_review() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(f._temp.path(), "race-install-src", "race-install", "1.0.0");
    let stable_src = write_skill_bundle(
        &f.config.workspace,
        "race-enable-src",
        "race-enable",
        "1.0.0",
    );
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    {
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let init = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": stable_src.to_string_lossy(), "enabled": false}),
            )
            .await
            .unwrap();
        assert_eq!(init["application"], "applied");
    }

    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let src_in_gate = src.clone();
    install_gate(&mut f, move |input, _| {
        let _ = fs::write(
            src_in_gate.join("skills/helper/SKILL.md"),
            "---\nname: helper\ndescription: raced\n---\nchanged\n",
        );
        allow_verdict(input, NestedDecision::Approved)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let install_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(install_res["application"], "failed", "{install_res}");
    assert_eq!(install_res["changed"], false);
    assert_eq!(install_res["error"]["diagnostic"], "changed after review");
    assert!(!f.config.user_plugins_dir.join("race-install").exists());

    let installed_dir = f.config.user_plugins_dir.join("race-enable");
    install_gate(&mut f, move |input, _| {
        fs::write(
            installed_dir.join("skills/helper/SKILL.md"),
            "---\nname: helper\ndescription: raced\n---\nchanged on disk\n",
        )
        .unwrap();
        allow_verdict(input, NestedDecision::Approved)
    });
    let guard2 = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket2 = plugin_ticket(&guard2).unwrap();
    let enable_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket2,
            "enable",
            json!({"target": "race-enable"}),
        )
        .await
        .unwrap();
    assert_eq!(enable_res["application"], "failed", "{enable_res}");
    assert_eq!(enable_res["changed"], false);
    assert_eq!(enable_res["error"]["diagnostic"], "changed after review");
    assert!(
        !discover_with_config(&f.config)
            .get("race-enable")
            .unwrap()
            .enabled
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_dial_moved_during_the_card_decides_who_is_recorded() {
    let mut f = PluginsFixture::new();
    let in_ws = write_skill_bundle(&f.config.workspace, "move-in-src", "move-in", "1.0.0");
    let out_ws = write_skill_bundle(f._temp.path(), "move-out-src", "move-out", "1.0.0");
    let hook_ws = write_skill_bundle(&f.config.workspace, "move-hook-src", "move-hook", "1.0.0");
    f.reload_registry();

    f.posture.switch_for_tests(ApprovalMode::Suggest);
    install_gate(&mut f, |input, posture| {
        posture.switch_for_tests(ApprovalMode::Bypass);
        allow_verdict(input, NestedDecision::Approved)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let ask_to_full = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": in_ws.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(ask_to_full["application"], "applied", "{ask_to_full}");
    assert!(
        ask_to_full["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "approved by the agent in Full Access"),
        "{ask_to_full}"
    );
    let reg = discover_with_config(&f.config);
    assert!(matches!(
        reg.get("move-in").unwrap().approved_by,
        Some(codewhale_plugins::host_services::PluginApprover::Agent(_))
    ));

    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let ask_to_full_outside = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": out_ws.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(
        ask_to_full_outside["application"], "failed",
        "{ask_to_full_outside}"
    );
    assert_eq!(ask_to_full_outside["changed"], false);
    assert!(!f.config.user_plugins_dir.join("move-out").exists());

    f.posture.switch_for_tests(ApprovalMode::Bypass);
    let posture_for_hook = f.posture.clone();
    super::set_post_install_hook_for_test(Box::new(move |_, _| {
        posture_for_hook.switch_for_tests(ApprovalMode::Suggest);
    }));
    let full_to_ask = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": hook_ws.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(full_to_ask["application"], "applied", "{full_to_ask}");
    assert!(
        full_to_ask["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "approved by you"),
        "{full_to_ask}"
    );
    let reg2 = discover_with_config(&f.config);
    assert!(matches!(
        reg2.get("move-hook").unwrap().approved_by,
        Some(codewhale_plugins::host_services::PluginApprover::Person { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_sub_agent_caller_is_refused_on_changing_methods() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let mut sub_context = f.context.clone();
    sub_context.execution.owner_agent_id = Some("sub-agent-1".into());
    let sub_guard = f.begin(&f.creator, HostCohort::Builtin, &sub_context);
    assert!(plugin_ticket(&sub_guard).is_none());

    let root_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let root_ticket = plugin_ticket(&root_guard).unwrap();
    drop(root_guard);

    for method in ["install", "remove", "enable", "disable"] {
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &root_ticket,
                method,
                json!({"target": "alpha"}),
            )
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::REFUSED, "ticket_refused".into()),
            "{method}"
        );
    }
    assert!(sub_guard.receipts().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_plugin_tier_owner_is_refused_on_changing_methods() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let creator_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let creator_ticket = plugin_ticket(&creator_guard).unwrap();
    let forged_owner = OwnerRef {
        plugin_id: "host:creator".into(),
        generation: 99,
        owner_token: "forged-token".into(),
    };

    for method in ["install", "remove", "enable", "disable"] {
        for (owner, tier) in [
            (&f.plugin_owner, HostCohort::reviewed()),
            (&f.mcp, HostCohort::Builtin),
            (&forged_owner, HostCohort::reviewed()),
        ] {
            let err = f
                .call_broker(
                    owner,
                    tier,
                    &creator_ticket,
                    method,
                    json!({"target": "alpha"}),
                )
                .await;
            assert_eq!(
                refusal(&err),
                (error_code::REFUSED, "ticket_refused".into()),
                "{owner:?} {method}"
            );
        }

        for owner in [&f.plugin_owner, &f.mcp] {
            let arm_err = serve(detached_call(
                owner.clone(),
                method,
                json!({"target": "alpha"}),
            ))
            .await;
            assert!(
                matches!(
                    arm_err,
                    Err(super::super::dsh_broker::BrokerRefusal::Refused {
                        service: DshService::Plugins,
                        ..
                    })
                ),
                "{owner:?} {method}: {arm_err:?}"
            );
        }
    }
    assert!(creator_guard.receipts().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn the_managed_policy_answers_overridden() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "policy-src", "policy-plug", "1.0.0");
    let src2 = write_skill_bundle(
        &f.config.workspace,
        "policy-src-2",
        "policy-plug-2",
        "1.0.0",
    );
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let installed_off = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src2.to_string_lossy(), "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(installed_off["application"], "applied");

    let policy_path = f.config.state_path.with_file_name("managed-policy.json");
    fs::write(
        &policy_path,
        r#"{"schema_version":1,"allowed_plugins":[],"allow_unlisted":false}"#,
    )
    .unwrap();

    let overridden_install = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(
        overridden_install["application"], "overridden",
        "{overridden_install}"
    );
    assert_eq!(overridden_install["stage"], "enable");
    assert_eq!(overridden_install["changed"], true);
    assert_eq!(overridden_install["enabled"], false);
    assert_eq!(overridden_install["error"]["code"], "operation-error");
    assert!(
        overridden_install["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("managed plugin policy"),
        "{overridden_install}"
    );

    let overridden_enable = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "enable",
            json!({"target": "policy-plug-2"}),
        )
        .await
        .unwrap();
    assert_eq!(
        overridden_enable["application"], "overridden",
        "{overridden_enable}"
    );
    assert_eq!(overridden_enable["stage"], "enable");
    assert_eq!(overridden_enable["changed"], false);
    assert_eq!(overridden_enable["enabled"], false);
    assert_eq!(overridden_enable["error"]["code"], "operation-error");
}

#[tokio::test(flavor = "current_thread")]
async fn removal_keeps_the_data_folder_and_says_where() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "rem-src", "rem-plug", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let inst = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(inst["application"], "applied");

    let reg = discover_with_config(&f.config);
    let plugin_id = reg.get("rem-plug").unwrap().id.as_str().to_string();
    let data_dir =
        super::super::supervisor::plugin_data_dir(f._temp.path(), &plugin_id, "rem-plug");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(data_dir.join("keep.txt"), "user state").unwrap();

    let removed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "rem-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(removed["application"], "applied", "{removed}");
    assert_eq!(removed["stage"], "remove");
    assert_eq!(removed["changed"], true);
    assert_eq!(removed["enabled"], false);
    assert_eq!(removed["bundle"], "rem-plug");
    let expected_leaf = data_dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        removed["warnings"].as_array().unwrap().iter().any(|w| {
            w.as_str().is_some_and(|line| {
                line.contains("~/.codewhale/extension-host/data/plugins/")
                    && line.contains(&expected_leaf)
            })
        }),
        "{removed}"
    );
    assert!(data_dir.join("keep.txt").is_file());
    assert!(!f.config.user_plugins_dir.join("rem-plug").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn no_second_store() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "store-src", "store-plug", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "applied");

    assert!(f.config.state_path.is_file());
    assert!(
        f.config
            .user_plugins_dir
            .join("store-plug")
            .join(INSTALLED_FROM_MARKER)
            .is_file()
    );
    let state_parent = f.config.state_path.parent().unwrap();
    let files: Vec<String> = fs::read_dir(state_parent)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        files
            .iter()
            .all(|name| name.starts_with("plugin-state") || name == ".runtime"),
        "{files:?}"
    );

    let rediscovered = discover_with_config(&f.config);
    let loaded = rediscovered.get("store-plug").unwrap();
    assert!(loaded.enabled);
    assert!(loaded.trusted());
    assert!(loaded.install_nonce.is_some());

    let off = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "disable",
            json!({"target": "store-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(off["application"], "applied", "{off}");
    let removed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "store-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(removed["application"], "applied", "{removed}");
    let after_remove_files: Vec<String> = fs::read_dir(state_parent)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        after_remove_files
            .iter()
            .all(|name| name.starts_with("plugin-state") || name == ".runtime"),
        "{after_remove_files:?}"
    );
    let user_entries: Vec<String> = fs::read_dir(&f.config.user_plugins_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(user_entries.is_empty(), "{user_entries:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn the_agent_cannot_turn_off_or_remove_a_record_not_its_own_and_a_person_can() {
    use codewhale_plugins::host_services::{
        AgentApproval, AgentPosture, PersonVia, PluginApprover, ReviewApproval,
    };

    let mut f = PluginsFixture::new();
    let person_dir = write_skill_bundle(
        &f.config.user_plugins_dir,
        "person-plug",
        "person-plug",
        "1.0.0",
    );
    let external_src = write_skill_bundle(f._temp.path(), "person-src", "person-plug", "1.0.0");
    fs::write(
        person_dir.join(INSTALLED_FROM_MARKER),
        json!({
            "schema_version": 2,
            "spec": format!("path:{}", external_src.display()),
            "url": null,
            "source_checksum": "",
            "content_digest": "",
            "installed_name": "person-plug",
            "registry_version": null,
            "install_nonce": "nonce-person",
        })
        .to_string(),
    )
    .unwrap();

    let other_ws_dir = write_skill_bundle(
        &f.config.user_plugins_dir,
        "other-proj-plug",
        "other-proj-plug",
        "1.0.0",
    );
    let other_ws_src = f._temp.path().join("other-project/src");
    fs::create_dir_all(&other_ws_src).unwrap();
    fs::write(
        other_ws_dir.join(INSTALLED_FROM_MARKER),
        json!({
            "schema_version": 2,
            "spec": format!("path:{}", other_ws_src.display()),
            "url": null,
            "source_checksum": "",
            "content_digest": "",
            "installed_name": "other-proj-plug",
            "registry_version": null,
            "install_nonce": "nonce-other",
        })
        .to_string(),
    )
    .unwrap();

    let cli_dir = write_skill_bundle(&f.config.user_plugins_dir, "cli-plug", "cli-plug", "1.0.0");
    fs::write(
        cli_dir.join(INSTALLED_FROM_MARKER),
        json!({
            "schema_version": 2,
            "spec": "path:/tmp/cli",
            "url": null,
            "source_checksum": "",
            "content_digest": "",
            "installed_name": "cli-plug",
            "registry_version": null,
            "install_nonce": "nonce-cli",
        })
        .to_string(),
    )
    .unwrap();

    let no_marker_dir = write_skill_bundle(
        &f.config.user_plugins_dir,
        "legacy-plug",
        "legacy-plug",
        "1.0.0",
    );
    let _ = no_marker_dir;

    let mut reg = discover_with_config(&f.config);
    approve_reviewed(
        &mut reg,
        "person-plug",
        PluginApprover::person(PersonVia::ApprovalCard),
    );
    let other_agent = PluginApprover::Agent(AgentApproval {
        posture: AgentPosture::FullAccess,
        workspace: f
            ._temp
            .path()
            .join("other-project")
            .to_string_lossy()
            .into_owned(),
        thread_id: Some("t-other".into()),
        turn_id: Some("u-other".into()),
        tool_call_id: Some("c-other".into()),
    });
    let other_id = reg.get("other-proj-plug").unwrap().id.as_str().to_string();
    let other_host_review = reg
        .host_service_review_for(&other_id, &other_agent)
        .unwrap();
    let mut reg_other = DiscoveryConfig {
        workspace: f._temp.path().join("other-project"),
        workspace_plugins_dir: f._temp.path().join("other-project/.codewhale/plugins"),
        ..f.config.clone()
    };
    let mut other_reg = discover_with_config(&reg_other);
    other_reg
        .trust_reviewed(
            &other_id,
            ReviewApproval {
                approved_by: other_agent.clone(),
                host_services: Some(other_host_review.token),
            },
        )
        .unwrap();
    other_reg.enable_by(&other_id, other_agent).unwrap();

    reg_other.workspace = f.config.workspace.clone();
    let mut reg = discover_with_config(&f.config);
    approve_reviewed(&mut reg, "cli-plug", PluginApprover::CommandLine);
    approve_reviewed(
        &mut reg,
        "legacy-plug",
        PluginApprover::person(PersonVia::Terminal),
    );

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for (method, target) in [
        ("disable", "person-plug"),
        ("remove", "person-plug"),
        ("disable", "other-proj-plug"),
        ("remove", "other-proj-plug"),
        ("disable", "cli-plug"),
        ("remove", "cli-plug"),
        ("disable", "legacy-plug"),
    ] {
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                method,
                json!({"target": target}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "failed", "{method} {target}: {res}");
        assert_eq!(res["changed"], false, "{method} {target}: {res}");
        assert!(
            res["error"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("ask the person: step to Ask, or use the Plugins page"),
            "{method} {target}: {res}"
        );
    }

    let legacy_remove = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "legacy-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(legacy_remove["error"]["code"], "not-removable");
    assert_eq!(f.asked.load(Ordering::SeqCst), 0);

    let saved_state = fs::read(&f.config.state_path).unwrap();
    fs::write(&f.config.state_path, "{corrupt json").unwrap();
    f.reload_registry();
    let corrupt_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let corrupt_ticket = plugin_ticket(&corrupt_guard).unwrap();
    for method in ["disable", "remove"] {
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &corrupt_ticket,
                method,
                json!({"target": "person-plug"}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "failed", "corrupt {method}: {res}");
        assert!(
            res["error"]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("no valid review record"),
            "corrupt {method}: {res}"
        );
    }
    assert_eq!(f.asked.load(Ordering::SeqCst), 0);
    fs::write(&f.config.state_path, saved_state).unwrap();
    f.reload_registry();

    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let ask_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ask_ticket = plugin_ticket(&ask_guard).unwrap();

    for (method, target) in [
        ("disable", "person-plug"),
        ("enable", "person-plug"),
        ("remove", "person-plug"),
        ("disable", "other-proj-plug"),
        ("remove", "cli-plug"),
    ] {
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ask_ticket,
                method,
                json!({"target": target}),
            )
            .await
            .unwrap();
        assert_eq!(
            res["application"], "applied",
            "person {method} {target}: {res}"
        );
    }
    assert_eq!(f.asked.load(Ordering::SeqCst), 5);
}

#[tokio::test(flavor = "current_thread")]
async fn remove_then_install_of_the_same_bytes_is_a_new_record() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "reinst-src", "reinst-plug", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let first = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(first["application"], "applied");
    let nonce1 = discover_with_config(&f.config)
        .get("reinst-plug")
        .unwrap()
        .install_nonce
        .clone()
        .unwrap();

    let removed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "reinst-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(removed["application"], "applied");

    let second = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(second["application"], "applied", "{second}");
    let reg2 = discover_with_config(&f.config);
    let loaded2 = reg2.get("reinst-plug").unwrap();
    let nonce2 = loaded2.install_nonce.clone().unwrap();
    assert_ne!(nonce1, nonce2);
    assert!(loaded2.enabled);
}

#[tokio::test(flavor = "current_thread")]
async fn an_agent_approved_plugin_with_code_runs_in_agent_cohort_and_promotes_on_review() {
    let Some(node) = super::super::test_support::node_for_tests(
        "an_agent_approved_plugin_with_code_runs_in_agent_cohort_and_promotes_on_review",
    ) else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    f.manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(f._temp.path().join("host-root")),
        ..Default::default()
    }));
    {
        let mut reg = f.manager.shared.registry.lock().unwrap();
        f.creator = reg
            .begin_owner(
                HostCohort::Builtin,
                "host:creator",
                "creator",
                None,
                "digest",
            )
            .unwrap();
        reg.mark_active(&f.creator);
    }
    let _manager = TestManagerGuard::install(Arc::clone(&f.manager));
    let initial_reg = f.reload_registry();
    let _engine = f.manager.attach(initial_reg);
    let _ = _engine.set_creator_demand(true);

    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let src = write_native_bundle(&f.config.workspace, "native-src", "native-plug", "1.0.0");

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let agent_install = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(agent_install["application"], "applied", "{agent_install}");
    assert_eq!(agent_install["changed"], true);
    assert_eq!(agent_install["enabled"], true);
    assert!(agent_install.get("error").is_none(), "{agent_install}");
    let plugin_id = discover_with_config(&f.config)
        .get("native-plug")
        .unwrap()
        .id
        .as_str()
        .to_string();
    assert_eq!(f.manager.owner_state(&plugin_id), Some(OwnerState::Active));
    assert_eq!(
        f.manager
            .owner_report(&plugin_id)
            .and_then(|report| report.cohort),
        Some(HostCohort::agent(&f.config.workspace))
    );

    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let person_enable = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "enable",
            json!({"target": "native-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(person_enable["application"], "applied", "{person_enable}");
    assert_eq!(person_enable["enabled"], true);
    assert!(
        person_enable["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "approved by you"),
        "{person_enable}"
    );
    assert_eq!(f.manager.owner_state(&plugin_id), Some(OwnerState::Active));
    assert_eq!(
        f.manager
            .owner_report(&plugin_id)
            .and_then(|report| report.cohort),
        Some(HostCohort::reviewed())
    );

    f.manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_plugin_without_code_is_applied() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let src = write_skill_bundle(&f.config.workspace, "no-code-src", "no-code-plug", "1.0.0");

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "applied", "{res}");
    assert_eq!(res["enabled"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn a_remote_source_is_not_fetched_before_the_gate() {
    use codewhale_net::network_policy::{NetworkPolicy, NetworkPolicyDecider};

    let mut f = PluginsFixture::new();
    f.reload_registry();

    for (label, mode, approval_mode) in [
        ("never", AppMode::Agent, ApprovalMode::Never),
        ("plan", AppMode::Plan, ApprovalMode::Suggest),
    ] {
        f.posture.switch_mode_for_tests(mode, approval_mode);
        let card_prompts = Arc::new(AtomicUsize::new(0));
        let card_prompts_in_gate = Arc::clone(&card_prompts);
        let gate_calls = install_gate(&mut f, move |_, posture| {
            let live = posture.read();
            if live.mode() == AppMode::Plan || live.approval_mode() == ApprovalMode::Never {
                return deny_verdict("should not ask in plan or never");
            }
            card_prompts_in_gate.fetch_add(1, Ordering::SeqCst);
            deny_verdict("unexpected")
        });
        let plan_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let plan_ticket = plugin_ticket(&plan_guard).unwrap();
        let plan_err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &plan_ticket,
                "install",
                json!({"target": "github:nonexistent-owner-zz/nonexistent-repo-zz"}),
            )
            .await;
        assert_eq!(
            refusal(&plan_err),
            (error_code::DENIED, "denied".into()),
            "{label}"
        );
        assert_eq!(card_prompts.load(Ordering::SeqCst), 0, "{label}");
        assert_eq!(gate_calls.lock().unwrap().len(), 1, "{label}");
    }

    f.posture
        .switch_mode_for_tests(AppMode::Agent, ApprovalMode::Suggest);
    f.asked.store(0, Ordering::SeqCst);
    let seen = install_gate(&mut f, |_, _| deny_verdict("denied remote fetch"));
    let ask_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ask_ticket = plugin_ticket(&ask_guard).unwrap();
    let ask_err = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ask_ticket,
            "install",
            json!({"target": "https://127.0.0.1:9/never-fetched.tgz"}),
        )
        .await;
    assert_eq!(refusal(&ask_err), (error_code::DENIED, "denied".into()));
    assert_eq!(f.asked.load(Ordering::SeqCst), 1);
    assert_eq!(
        seen.lock().unwrap()[0].1.as_deref(),
        Some("https://127.0.0.1:9/never-fetched.tgz from 127.0.0.1")
    );

    let blocked_ctx = f
        .context
        .clone()
        .with_network_policy(NetworkPolicyDecider::new(NetworkPolicy::default(), None));
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let policy_guard = f.begin(&f.creator, HostCohort::Builtin, &blocked_ctx);
    let policy_ticket = plugin_ticket(&policy_guard).unwrap();
    let policy_blocked = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &policy_ticket,
            "install",
            json!({"target": "https://127.0.0.1:9/never-fetched.tgz"}),
        )
        .await
        .unwrap();
    assert_eq!(policy_blocked["application"], "failed", "{policy_blocked}");
    assert_eq!(policy_blocked["changed"], false, "{policy_blocked}");
    assert_eq!(policy_blocked["error"]["code"], "operation-error");
}

#[tokio::test(flavor = "current_thread")]
async fn a_row_that_is_not_the_only_row_is_refused_and_a_sole_row_switches_the_plugin() {
    let mut f = PluginsFixture::new();
    let two_src = write_toml_plugin(
        &f.config.workspace,
        "two-src",
        "schema_version = 1\n[plugin]\nname = \"two\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n[commands]\npath = \"commands\"\n",
    );
    write_skill(&two_src, "helper");
    fs::create_dir_all(two_src.join("commands")).unwrap();
    fs::write(two_src.join("commands/extra.md"), "extra command\n").unwrap();
    let one_src = write_toml_plugin(
        &f.config.workspace,
        "one-src",
        "schema_version = 1\n[plugin]\nname = \"one\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n",
    );
    write_skill(&one_src, "helper");

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    for src in [&two_src, &one_src] {
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": src.to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "applied", "{res}");
    }

    for method in ["disable", "enable"] {
        for payload in [
            json!({"target": "two", "row": "two/skills"}),
            json!({"target": "two/skills"}),
        ] {
            let res = f
                .call_broker(&f.creator, HostCohort::Builtin, &ticket, method, payload)
                .await
                .unwrap();
            assert_eq!(res["application"], "failed", "{method}: {res}");
            assert_eq!(res["changed"], false);
            assert_eq!(
                res["error"]["diagnostic"],
                "Codewhale switches a plugin as a whole; use set_bundle"
            );
        }

        let missing_row = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                method,
                json!({"target": "two", "row": "two/nonexistent"}),
            )
            .await
            .unwrap();
        assert_eq!(
            missing_row["application"], "failed",
            "{method}: {missing_row}"
        );
        assert_eq!(
            missing_row["error"]["code"], "unknown-plugin",
            "{missing_row}"
        );
    }

    let off = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "disable",
            json!({"target": "one", "row": "one/skills"}),
        )
        .await
        .unwrap();
    assert_eq!(off["application"], "applied", "{off}");
    assert_eq!(off["stage"], "disable", "{off}");
    assert_eq!(off["changed"], true);
    assert_eq!(off["enabled"], false);

    let on = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "enable",
            json!({"target": "one/skills"}),
        )
        .await
        .unwrap();
    assert_eq!(on["application"], "applied", "{on}");
    assert_eq!(on["stage"], "enable", "{on}");
    assert_eq!(on["changed"], true);
    assert_eq!(on["enabled"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn approved_builds_answers_stale_approval_and_invalid_specs_fail() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "stale-src", "stale-plug", "1.0.0");
    let dup = write_skill_bundle(&f.config.workspace, "stale-dup", "stale-plug", "2.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let stale = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy(), "approved_builds": ["dep-a"]}),
        )
        .await
        .unwrap();
    assert_eq!(stale["application"], "failed", "{stale}");
    assert_eq!(stale["changed"], false);
    assert_eq!(stale["error"]["code"], "stale-approval");

    let reg_warn = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy(), "registry": "https://registry.example"}),
        )
        .await
        .unwrap();
    assert_eq!(reg_warn["application"], "applied", "{reg_warn}");
    assert!(
        reg_warn["warnings"].as_array().unwrap().iter().any(|w| {
            w == "registry was ignored; Codewhale installs from the configured source"
        }),
        "{reg_warn}"
    );

    let already = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": dup.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(already["application"], "failed", "{already}");
    assert_eq!(already["changed"], false);
    assert_eq!(already["error"]["code"], "already-installed");

    for bad in [
        "@scope/pkg@^1.0.0",
        "npm:pkg@^1.0.0",
        "http://example.com/pkg.tgz",
        "relative/dir",
    ] {
        let invalid = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": bad}),
            )
            .await
            .unwrap();
        assert_eq!(invalid["application"], "failed", "{bad}: {invalid}");
        assert_eq!(invalid["changed"], false, "{bad}: {invalid}");
        assert_eq!(invalid["error"]["code"], "invalid-spec", "{bad}: {invalid}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_recorded_source_path_reads_back_for_update() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "upd-src", "upd-plug", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let v1 = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(v1["application"], "applied");

    let recorded_source = read_install_receipt(&f.config.user_plugins_dir.join("upd-plug"))["spec"]
        .as_str()
        .unwrap()
        .to_string();
    let recorded_path = recorded_source
        .strip_prefix("path:")
        .unwrap_or_else(|| panic!("expected path: spec, got {recorded_source}"));
    assert_eq!(
        fs::canonicalize(recorded_path).unwrap(),
        fs::canonicalize(&src).unwrap()
    );

    fs::write(
        src.join("plugin.toml"),
        "schema_version = 1\n[plugin]\nname = \"upd-plug\"\nversion = \"2.0.0\"\n",
    )
    .unwrap();
    let rm = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "upd-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(rm["application"], "applied");

    let v2 = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": recorded_source}),
        )
        .await
        .unwrap();
    assert_eq!(v2["application"], "applied", "{v2}");
    assert_eq!(v2["version"], "2.0.0");
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_half_done_removal_missing_identity_and_oversized_bundle() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "edge-src", "edge-plug", "1.0.0");
    let big_src = write_skill_bundle(&f.config.workspace, "big-src", "big-plug", "1.0.0");
    fs::write(
        big_src.join("payload.bin"),
        vec![b'x'; (codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES as usize) + 1024],
    )
    .unwrap();

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let mut no_tool_call = f.context.clone();
    no_tool_call.origin_tool_call_id = None;
    let nt_guard = f.begin(&f.creator, HostCohort::Builtin, &no_tool_call);
    let nt_ticket = plugin_ticket(&nt_guard).unwrap();
    let missing_id = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &nt_ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(missing_id["application"], "failed", "{missing_id}");
    assert_eq!(missing_id["changed"], false);
    assert!(!f.config.user_plugins_dir.join("edge-plug").exists());
    drop(nt_guard);

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let too_big = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": big_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(too_big["application"], "failed", "{too_big}");
    assert_eq!(too_big["changed"], false);
    assert!(!f.config.user_plugins_dir.join("big-plug").exists());

    super::set_post_install_hook_for_test(Box::new(|_, cancel| {
        cancel.cancel();
    }));
    let cancelled = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await;
    assert_eq!(
        refusal(&cancelled),
        (error_code::CANCELLED, "cancelled".into())
    );
    let loaded_after_cancel = discover_with_config(&f.config)
        .get("edge-plug")
        .unwrap()
        .clone();
    assert!(!loaded_after_cancel.enabled);
    assert!(!loaded_after_cancel.trusted());

    let rm_src = write_skill_bundle(&f.config.workspace, "half-src", "half-plug", "1.0.0");
    let half_inst = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": rm_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(half_inst["application"], "applied");
    super::set_post_disable_hook_for_test(Box::new(|plugin| {
        let _ = fs::remove_file(plugin.canonical_root.join(INSTALLED_FROM_MARKER));
    }));
    let half_rm = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "half-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(half_rm["application"], "failed", "{half_rm}");
    assert_eq!(half_rm["changed"], true);
    assert_eq!(half_rm["enabled"], false);
    assert!(
        half_rm["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .starts_with("plugin was turned off, then removal failed:"),
        "{half_rm}"
    );
    assert!(
        !discover_with_config(&f.config)
            .get("half-plug")
            .unwrap()
            .enabled
    );
}

#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn a_drive_letter_target_is_inside_the_project_on_canonical_paths() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "win-drive-src", "win-drive", "1.0.0");
    let raw = src.to_string_lossy().into_owned();
    let flipped = if raw.starts_with(|c: char| c.is_ascii_alphabetic())
        && raw.as_bytes().get(1) == Some(&b':')
    {
        let mut chars: Vec<char> = raw.chars().collect();
        chars[0] = if chars[0].is_ascii_uppercase() {
            chars[0].to_ascii_lowercase()
        } else {
            chars[0].to_ascii_uppercase()
        };
        chars.into_iter().collect()
    } else {
        raw
    };
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": flipped}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "applied", "{res}");
}

#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn a_junction_in_the_source_is_refused() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "win-junc-src", "win-junc", "1.0.0");
    let outside = f._temp.path().join("win-outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "secret").unwrap();
    let junction = src.join("junc");
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&junction)
        .arg(&outside)
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "failed", "{res}");
    assert_eq!(res["changed"], false);
    assert!(!f.config.user_plugins_dir.join("win-junc").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn fresh_home_without_user_plugins_dir_installs_local_project_plugin() {
    let mut f = PluginsFixture::new();
    fs::remove_dir_all(&f.config.user_plugins_dir).unwrap();
    let src = write_skill_bundle(&f.config.workspace, "fresh-home-src", "fresh-home", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "applied", "{res}");
    assert_eq!(res["changed"], true);
    assert_eq!(res["enabled"], true);
    let installed_dir = f.config.user_plugins_dir.join("fresh-home");
    assert!(installed_dir.is_dir());
    let receipt = read_install_receipt(&installed_dir);
    assert!(
        receipt["install_nonce"]
            .as_str()
            .is_some_and(|n| !n.is_empty()),
        "{receipt}"
    );
    drop(guard);
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let listed = f
        .call_broker(&f.creator, HostCohort::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let entry = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "fresh-home")
        .unwrap();
    assert_eq!(entry["codewhale"]["approved_by"]["kind"], "agent");
    assert_eq!(entry["codewhale"]["approved_by"]["posture"], "full_access");
    assert_eq!(entry["codewhale"]["approved_by"]["this_project"], true);
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn dangling_user_plugins_dir_symlink_fails_closed_in_agent_mode() {
    let mut f = PluginsFixture::new();
    fs::remove_dir_all(&f.config.user_plugins_dir).unwrap();
    let missing_target = f._temp.path().join("missing-user-plugins-target");
    std::os::unix::fs::symlink(&missing_target, &f.config.user_plugins_dir).unwrap();
    let src = write_skill_bundle(
        &f.config.workspace,
        "dangling-up-src",
        "dangling-up",
        "1.0.0",
    );
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "failed", "{res}");
    assert_eq!(res["changed"], false);
    assert_eq!(res["error"]["code"], "operation-error");
    assert!(!missing_target.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn plan_and_never_skip_fs_probes_and_nonexistent_path_does_not_leak() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let nonexistent = f._temp.path().join("does-not-exist-anywhere/plugin");
    assert!(!nonexistent.exists());

    for (label, mode, approval_mode) in [
        ("never", AppMode::Agent, ApprovalMode::Never),
        ("plan", AppMode::Plan, ApprovalMode::Suggest),
    ] {
        f.posture.switch_mode_for_tests(mode, approval_mode);
        let gate_calls = install_gate(&mut f, |_, posture| {
            let live = posture.read();
            if live.mode() == AppMode::Plan || live.approval_mode() == ApprovalMode::Never {
                return deny_verdict("blocked by plan or never posture");
            }
            deny_verdict("unexpected")
        });
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": nonexistent.to_string_lossy()}),
            )
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::DENIED, "denied".into()),
            "{label}"
        );
        let calls = gate_calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "{label}");
        assert_eq!(
            calls[0].1.as_deref(),
            Some(nonexistent.to_string_lossy().as_ref()),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ask_defers_external_non_bundle_and_symlink_ancestor_until_after_write_gate() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);

    let nonexistent = f._temp.path().join("external-nonexistent/plugin");
    let non_bundle = f._temp.path().join("external-empty-dir");
    fs::create_dir_all(&non_bundle).unwrap();
    let bad_toml_dir = write_toml_plugin(
        f._temp.path(),
        "external-bad-toml",
        "this is not = [valid toml",
    );

    let targets: Vec<(String, &'static str)> = [
        (
            nonexistent.to_string_lossy().into_owned(),
            "operation-error",
        ),
        (non_bundle.to_string_lossy().into_owned(), "not-bundle"),
        (bad_toml_dir.to_string_lossy().into_owned(), "not-bundle"),
    ]
    .into_iter()
    .chain({
        #[cfg(unix)]
        {
            let via_ancestor_link = f._temp.path().join("ancestor-wrap");
            fs::create_dir_all(&via_ancestor_link).unwrap();
            let _linked_bundle =
                write_skill_bundle(&via_ancestor_link, "nested", "ancestor-linked", "1.0.0");
            let parent_link = f._temp.path().join("parent-symlink");
            std::os::unix::fs::symlink(&via_ancestor_link, &parent_link).unwrap();
            Some((
                parent_link.join("nested").to_string_lossy().into_owned(),
                "operation-error",
            ))
        }
        #[cfg(not(unix))]
        {
            None
        }
    })
    .collect();

    for (raw_target, _) in &targets {
        let gate_calls = install_gate(&mut f, |_, _| deny_verdict("denied before inspection"));
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": raw_target}),
            )
            .await;
        assert_eq!(refusal(&err), (error_code::DENIED, "denied".into()));
        let calls = gate_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.as_deref(), Some(raw_target.as_str()));
    }

    for (raw_target, expected_code) in &targets {
        let gate_calls = install_gate(&mut f, |input, _| {
            allow_verdict(input, NestedDecision::Approved)
        });
        let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
        let ticket = plugin_ticket(&guard).unwrap();
        let res = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "install",
                json!({"target": raw_target}),
            )
            .await
            .unwrap();
        assert_eq!(res["application"], "failed", "{raw_target}: {res}");
        assert_eq!(res["changed"], false);
        assert_eq!(res["error"]["code"], *expected_code, "{raw_target}: {res}");
        let calls = gate_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.as_deref(), Some(raw_target.as_str()));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn local_dsh_with_unresolved_npm_deps_gates_reach_refuses_agent_and_denies_prompt_policy_without_network()
 {
    use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};

    let mut f = PluginsFixture::new();
    let dsh_src = f.config.workspace.join("dsh-npm-src");
    copy_dir_tree(&dsh_template_dir("decoration"), &dsh_src);
    let mut pkg_json: Value =
        serde_json::from_str(&fs::read_to_string(dsh_src.join("package.json")).unwrap()).unwrap();
    pkg_json["dependencies"] = json!({"zod": "^4.0.0"});
    fs::write(
        dsh_src.join("package.json"),
        serde_json::to_string_pretty(&pkg_json).unwrap(),
    )
    .unwrap();
    fs::write(
        dsh_src.join("index.js"),
        "import { z } from 'zod';\nexport function apply() { return z; }\n",
    )
    .unwrap();
    f.reload_registry();

    f.posture.switch_for_tests(ApprovalMode::Bypass);
    let agent_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let agent_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let agent_ticket = plugin_ticket(&agent_guard).unwrap();
    let agent_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &agent_ticket,
            "install",
            json!({"target": dsh_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(agent_res["application"], "failed", "{agent_res}");
    assert_eq!(agent_res["changed"], false);
    assert_eq!(agent_res["error"]["code"], "operation-error");
    assert!(
        agent_res["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("registry.npmjs.org"),
        "{agent_res}"
    );
    assert_eq!(agent_calls.lock().unwrap().len(), 0);

    f.posture.switch_for_tests(ApprovalMode::Suggest);
    let expected_reach = format!(
        "{} npm dependencies from registry.npmjs.org",
        dsh_src.to_string_lossy()
    );
    let deny_reach_calls = install_gate(&mut f, |_, _| deny_verdict("denied npm reach"));
    let deny_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let deny_ticket = plugin_ticket(&deny_guard).unwrap();
    let reach_denied = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &deny_ticket,
            "install",
            json!({"target": dsh_src.to_string_lossy()}),
        )
        .await;
    assert_eq!(
        refusal(&reach_denied),
        (error_code::DENIED, "denied".into())
    );
    assert_eq!(
        deny_reach_calls.lock().unwrap().as_slice(),
        &[("dsh_broker_reach".to_string(), Some(expected_reach.clone()))]
    );

    let prompt_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let prompt_ctx = f
        .context
        .clone()
        .with_network_policy(NetworkPolicyDecider::new(NetworkPolicy::default(), None));
    let prompt_guard = f.begin(&f.creator, HostCohort::Builtin, &prompt_ctx);
    let prompt_ticket = plugin_ticket(&prompt_guard).unwrap();
    let prompt_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &prompt_ticket,
            "install",
            json!({"target": dsh_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(prompt_res["application"], "failed", "{prompt_res}");
    assert_eq!(prompt_res["changed"], false);
    assert_eq!(prompt_res["error"]["code"], "operation-error");
    assert!(
        prompt_res["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("network approval required for registry.npmjs.org"),
        "{prompt_res}"
    );
    assert_eq!(
        prompt_calls.lock().unwrap().as_slice(),
        &[("dsh_broker_reach".to_string(), Some(expected_reach.clone()))]
    );

    let deny_policy = NetworkPolicy {
        default: Decision::Deny.into(),
        ..NetworkPolicy::default()
    };
    let deny_policy_ctx = f
        .context
        .clone()
        .with_network_policy(NetworkPolicyDecider::new(deny_policy, None));
    let deny_policy_guard = f.begin(&f.creator, HostCohort::Builtin, &deny_policy_ctx);
    let deny_policy_ticket = plugin_ticket(&deny_policy_guard).unwrap();
    let deny_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &deny_policy_ticket,
            "install",
            json!({"target": dsh_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(deny_res["application"], "failed", "{deny_res}");
    assert_eq!(deny_res["changed"], false);
    assert!(
        deny_res["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("network access denied for registry.npmjs.org"),
        "{deny_res}"
    );
    assert!(!f.config.user_plugins_dir.join("my-decoration").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn remote_dial_move_during_reach_leaves_nothing_on_disk() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    install_gate(&mut f, |input, posture| {
        posture.switch_for_tests(ApprovalMode::Never);
        allow_verdict(input, NestedDecision::Approved)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": "https://127.0.0.1:9/remote.tgz"}),
        )
        .await
        .unwrap();
    assert_eq!(res["application"], "failed", "{res}");
    assert_eq!(res["changed"], false);
    assert_eq!(res["error"]["code"], "operation-error");
    assert_eq!(fs::read_dir(&f.config.user_plugins_dir).unwrap().count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn toggle_enable_missing_identity_and_managed_policy_upgrade_keep_state_atomic() {
    let mut f = PluginsFixture::new();
    let src = write_skill_bundle(&f.config.workspace, "atomic-src", "atomic-plug", "1.0.0");
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let installed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy(), "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(installed["application"], "applied", "{installed}");
    assert_eq!(installed["enabled"], false);
    drop(guard);

    let mut ctx_without_identity = f.context.clone();
    ctx_without_identity.origin_turn_id = None;
    ctx_without_identity.origin_tool_call_id = None;
    let missing_id_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let no_id_guard = f.begin(&f.creator, HostCohort::Builtin, &ctx_without_identity);
    let no_id_ticket = plugin_ticket(&no_id_guard).unwrap();
    let no_id_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &no_id_ticket,
            "enable",
            json!({"target": "atomic-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(no_id_res["application"], "failed", "{no_id_res}");
    assert_eq!(no_id_res["changed"], false);
    assert_eq!(no_id_res["enabled"], false);
    assert_eq!(no_id_res["error"]["code"], "operation-error");
    assert_eq!(missing_id_calls.lock().unwrap().len(), 0);
    drop(no_id_guard);

    let policy_path = f.config.state_path.with_file_name("managed-policy.json");
    fs::write(
        &policy_path,
        r#"{"schema_version":1,"allowed_plugins":[],"allow_unlisted":false}"#,
    )
    .unwrap();
    f.posture.switch_for_tests(ApprovalMode::Suggest);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let ask_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ask_ticket = plugin_ticket(&ask_guard).unwrap();
    let overridden = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ask_ticket,
            "enable",
            json!({"target": "atomic-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(overridden["application"], "overridden", "{overridden}");
    assert_eq!(overridden["changed"], false);
    assert_eq!(overridden["enabled"], false);

    let persisted = discover_with_config(&f.config);
    let loaded = persisted.get("atomic-plug").unwrap();
    assert!(!loaded.enabled);
    assert!(matches!(loaded.approved_by, Some(PluginApprover::Agent(_))));
}

#[tokio::test(flavor = "current_thread")]
async fn remove_rejects_ambiguous_target_non_sole_row_and_symlinked_install_marker() {
    let mut f = PluginsFixture::new();
    let two_src = write_toml_plugin(
        &f.config.workspace,
        "rm-two-src",
        "schema_version = 1\n[plugin]\nname = \"rm-two\"\nversion = \"1.0.0\"\n[skills]\npath = \"skills\"\n[commands]\npath = \"commands\"\n",
    );
    write_skill(&two_src, "helper");
    fs::create_dir_all(two_src.join("commands")).unwrap();
    fs::write(two_src.join("commands/extra.md"), "extra\n").unwrap();

    let dup_user = write_skill_bundle(
        &f.config.user_plugins_dir,
        "shared-user",
        "shared-plug",
        "1.0.0",
    );
    fs::write(dup_user.join(INSTALLED_FROM_MARKER), "local\n").unwrap();
    let _dup_ws = write_skill_bundle(
        &f.config.workspace_plugins_dir,
        "shared-ws",
        "shared-plug",
        "1.0.0",
    );

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    let inst_two = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": two_src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(inst_two["application"], "applied", "{inst_two}");

    let row_rm = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "rm-two/skills"}),
        )
        .await
        .unwrap();
    assert_eq!(row_rm["application"], "failed", "{row_rm}");
    assert_eq!(row_rm["error"]["code"], "operation-error");

    let ambig_rm = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "shared-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(ambig_rm["application"], "failed", "{ambig_rm}");
    assert_eq!(ambig_rm["error"]["code"], "operation-error");

    let ambig_en = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "enable",
            json!({"target": "shared-plug"}),
        )
        .await
        .unwrap();
    assert_eq!(ambig_en["application"], "failed", "{ambig_en}");
    assert_eq!(ambig_en["error"]["code"], "operation-error");

    #[cfg(unix)]
    {
        let target_marker = f._temp.path().join("outside-marker.json");
        fs::write(&target_marker, r#"{"approved_by":"agent"}"#).unwrap();
        let sym_dir = write_skill_bundle(
            &f.config.user_plugins_dir,
            "sym-marker",
            "sym-marker",
            "1.0.0",
        );
        std::os::unix::fs::symlink(&target_marker, sym_dir.join(INSTALLED_FROM_MARKER)).unwrap();
        f.reload_registry();
        let sym_rm = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                "remove",
                json!({"target": "sym-marker"}),
            )
            .await
            .unwrap();
        assert_eq!(sym_rm["application"], "failed", "{sym_rm}");
        assert_eq!(sym_rm["error"]["code"], "not-removable");
        assert!(sym_dir.is_dir());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn disable_and_remove_synchronously_revoke_tickets_and_cross_workspace_user_scope() {
    let _policy = TestPolicyGuard::extension_host(true);
    let mut f = PluginsFixture::new();
    let ws_b = f._temp.path().join("project-b");
    fs::create_dir_all(ws_b.join(".codewhale/plugins")).unwrap();
    let config_b = DiscoveryConfig {
        workspace: ws_b.clone(),
        user_plugins_dir: f.config.user_plugins_dir.clone(),
        workspace_plugins_dir: ws_b.join(".codewhale/plugins"),
        builtin_plugin_dirs: f.config.builtin_plugin_dirs.clone(),
        state_path: f.config.state_path.clone(),
    };

    let _manager = TestManagerGuard::install(Arc::clone(&f.manager));
    let reg_a = f.reload_registry();
    let _engine_a = f.manager.attach(reg_a);
    let _ = _engine_a.set_creator_demand(true);
    let _engine_b = f.manager.attach(Arc::new(discover_with_config(&config_b)));

    let src = write_skill_bundle(&f.config.workspace, "sync-rev-src", "sync-rev", "1.0.0");
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });

    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let inst = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": src.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(inst["application"], "applied", "{inst}");

    let live_reg = discover_with_config(&f.config);
    let loaded = live_reg.get("sync-rev").unwrap();
    let plugin_id = loaded.id.as_str().to_string();
    let live_auth = live_reg.authority_for(&plugin_id).unwrap();
    let b_snap = super::super::caller_view(&ws_b, None, None).unwrap();
    assert!(b_snap.get("sync-rev").is_some_and(|p| p.enabled));

    let plug_owner = {
        let mut reg = f.manager.shared.registry.lock().unwrap();
        let owner = reg
            .begin_owner(
                HostCohort::reviewed(),
                &plugin_id,
                "sync-rev",
                Some(live_auth.clone()),
                &loaded.content_hash,
            )
            .unwrap();
        reg.mark_active(&owner);
        owner
    };
    assert!(
        f.manager
            .owner_report_for(&plugin_id, Some(&live_auth))
            .is_some_and(|r| r.state == Some(OwnerState::Active))
    );
    assert!(
        f.manager
            .owner_report_for(&plugin_id, Some(&fake_authority("stale")))
            .is_none()
    );

    let tickets_before_plug = f.manager.shared.core_calls.live_tickets();
    let plug_guard = f.begin(&plug_owner, HostCohort::reviewed(), &f.context);
    let tickets_with_plug = f.manager.shared.core_calls.live_tickets();
    assert!(tickets_with_plug > tickets_before_plug);

    let disabled = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "disable",
            json!({"target": "sync-rev"}),
        )
        .await
        .unwrap();
    assert_eq!(disabled["application"], "applied", "{disabled}");
    assert_eq!(disabled["enabled"], false);

    assert!(
        f.manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owner(&plugin_id)
            .is_none()
    );
    assert_eq!(
        f.manager.shared.core_calls.live_tickets(),
        tickets_before_plug
    );
    let b_snap_after_disable = super::super::caller_view(&ws_b, None, None).unwrap();
    assert!(
        b_snap_after_disable
            .get("sync-rev")
            .is_some_and(|p| !p.enabled)
    );
    drop(plug_guard);

    let stale_owner = {
        let mut reg = f.manager.shared.registry.lock().unwrap();
        let owner = reg
            .begin_owner(
                HostCohort::reviewed(),
                &plugin_id,
                "sync-rev",
                Some(live_auth),
                &loaded.content_hash,
            )
            .unwrap();
        reg.mark_active(&owner);
        owner
    };
    let tickets_before_stale = f.manager.shared.core_calls.live_tickets();
    let stale_guard = f.begin(&stale_owner, HostCohort::reviewed(), &f.context);
    let tickets_before_rm = f.manager.shared.core_calls.live_tickets();
    assert!(tickets_before_rm > tickets_before_stale);

    let removed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "remove",
            json!({"target": "sync-rev"}),
        )
        .await
        .unwrap();
    assert_eq!(removed["application"], "applied", "{removed}");
    assert!(
        f.manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owner(&plugin_id)
            .is_none()
    );
    assert_eq!(
        f.manager.shared.core_calls.live_tickets(),
        tickets_before_stale
    );
    let b_snap_after_rm = super::super::caller_view(&ws_b, None, None).unwrap();
    assert!(b_snap_after_rm.get("sync-rev").is_none());
    drop(stale_guard);
    f.manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn remote_dsh_with_external_npm_deps_discloses_npm_reach_before_npm_access() {
    use codewhale_net::network_policy::{NetworkPolicy, NetworkPolicyDecider};
    use codewhale_skill_install::bounded_tar::test_archives::archive_from_entries;

    let pkg_json = serde_json::json!({
        "name": "@demo/remote-dsh",
        "version": "1.0.0",
        "main": "lib/index.js",
        "dependencies": { "zod": "^4.0.0" },
        "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
    })
    .to_string();
    let archive = archive_from_entries(&[
        ("dsh-remote/package.json", pkg_json.as_bytes()),
        (
            "dsh-remote/cordis.patch.yml",
            b"- insert:\n  - id: entry\n    name: ./lib/index.js\n",
        ),
        (
            "dsh-remote/lib/index.js",
            b"import { z } from 'zod';\nexport function apply() { return z; }\n",
        ),
    ]);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = vec![archive.clone(), archive];
    let server = std::thread::spawn(move || {
        for body in bodies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            loop {
                use std::io::Read as _;
                let read = stream.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            use std::io::Write as _;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });

    let url = format!("http://127.0.0.1:{port}/plugin.tar.gz");
    let expected_archive_reach = format!("{url} from 127.0.0.1");
    let expected_npm_reach = format!("{url} npm dependencies from registry.npmjs.org");

    let mut f = PluginsFixture::new();
    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Suggest);

    let net_policy = NetworkPolicy {
        allow: vec!["127.0.0.1".to_string()],
        ..NetworkPolicy::default()
    };
    let net_ctx = f
        .context
        .clone()
        .with_network_policy(NetworkPolicyDecider::new(net_policy, None));

    let prompt_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Approved)
    });
    let prompt_guard = f.begin(&f.creator, HostCohort::Builtin, &net_ctx);
    let prompt_ticket = plugin_ticket(&prompt_guard).unwrap();
    let prompt_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &prompt_ticket,
            "install",
            json!({"target": url}),
        )
        .await
        .unwrap();
    assert_eq!(prompt_res["application"], "failed", "{prompt_res}");
    assert_eq!(prompt_res["changed"], false);
    assert_eq!(prompt_res["error"]["code"], "operation-error");
    assert!(
        prompt_res["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("network approval required for registry.npmjs.org"),
        "{prompt_res}"
    );
    assert_eq!(
        prompt_calls.lock().unwrap().as_slice(),
        &[
            (
                "dsh_broker_reach".to_string(),
                Some(expected_archive_reach.clone())
            ),
            (
                "dsh_broker_reach".to_string(),
                Some(expected_npm_reach.clone())
            ),
        ]
    );
    assert_eq!(fs::read_dir(&f.config.user_plugins_dir).unwrap().count(), 0);

    let expected_npm_reach_for_deny = expected_npm_reach.clone();
    let deny_calls = install_gate(&mut f, move |input, _| {
        let target = input.get("target").and_then(Value::as_str).unwrap_or("");
        if target == expected_npm_reach_for_deny {
            deny_verdict("denied remote npm reach")
        } else {
            allow_verdict(input, NestedDecision::Approved)
        }
    });
    let deny_guard = f.begin(&f.creator, HostCohort::Builtin, &net_ctx);
    let deny_ticket = plugin_ticket(&deny_guard).unwrap();
    let deny_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &deny_ticket,
            "install",
            json!({"target": url}),
        )
        .await;
    assert_eq!(refusal(&deny_res), (error_code::DENIED, "denied".into()));
    assert_eq!(
        deny_calls.lock().unwrap().as_slice(),
        &[
            ("dsh_broker_reach".to_string(), Some(expected_archive_reach)),
            ("dsh_broker_reach".to_string(), Some(expected_npm_reach)),
        ]
    );
    assert_eq!(fs::read_dir(&f.config.user_plugins_dir).unwrap().count(), 0);
    let _ = server.join();
}

#[tokio::test(flavor = "current_thread")]
async fn plugins_install_prefers_dsh_package_when_plugin_json_also_present() {
    let mut f = PluginsFixture::new();

    let dsh_dual = f.config.workspace.join("dsh-dual-clean");
    copy_dir_tree(&dsh_template_dir("decoration"), &dsh_dual);
    let (conversion, _) = codewhale_plugins::install::preview_dsh(&dsh_dual).unwrap();
    let mut sibling = serde_json::Map::new();
    sibling.insert(
        "$schema".to_string(),
        Value::String(codewhale_plugins::agent_plugin::PLUGIN_SCHEMA_URL.to_string()),
    );
    sibling.insert(
        "name".to_string(),
        Value::String(conversion.plugin_name.clone()),
    );
    if let Some(version) = &conversion.source_version {
        sibling.insert("version".to_string(), Value::String(version.clone()));
    }
    fs::write(
        dsh_dual.join("plugin.json"),
        serde_json::to_string_pretty(&Value::Object(sibling)).unwrap(),
    )
    .unwrap();

    f.reload_registry();
    f.posture.switch_for_tests(ApprovalMode::Bypass);
    install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let installed = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &ticket,
            "install",
            json!({"target": dsh_dual.to_string_lossy(), "enabled": false}),
        )
        .await
        .unwrap();
    assert_eq!(installed["application"], "applied", "{installed}");
    assert_eq!(installed["changed"], true);
    let receipt = read_install_receipt(&f.config.user_plugins_dir.join(&conversion.plugin_name));
    assert!(
        receipt["spec"]
            .as_str()
            .is_some_and(|spec| spec.starts_with("dsh:")),
        "{receipt}"
    );
    drop(guard);

    let dsh_dual_npm = f.config.workspace.join("dsh-dual-npm");
    copy_dir_tree(&dsh_template_dir("decoration"), &dsh_dual_npm);
    let (npm_conv, _) = codewhale_plugins::install::preview_dsh(&dsh_dual_npm).unwrap();
    let mut npm_sibling = serde_json::Map::new();
    npm_sibling.insert(
        "$schema".to_string(),
        Value::String(codewhale_plugins::agent_plugin::PLUGIN_SCHEMA_URL.to_string()),
    );
    npm_sibling.insert(
        "name".to_string(),
        Value::String(npm_conv.plugin_name.clone()),
    );
    if let Some(version) = &npm_conv.source_version {
        npm_sibling.insert("version".to_string(), Value::String(version.clone()));
    }
    fs::write(
        dsh_dual_npm.join("plugin.json"),
        serde_json::to_string_pretty(&Value::Object(npm_sibling)).unwrap(),
    )
    .unwrap();
    let mut pkg_json: Value =
        serde_json::from_str(&fs::read_to_string(dsh_dual_npm.join("package.json")).unwrap())
            .unwrap();
    pkg_json["dependencies"] = json!({"zod": "^4.0.0"});
    fs::write(
        dsh_dual_npm.join("package.json"),
        serde_json::to_string_pretty(&pkg_json).unwrap(),
    )
    .unwrap();
    fs::write(
        dsh_dual_npm.join("index.js"),
        "import { z } from 'zod';\nexport function apply() { return z; }\n",
    )
    .unwrap();

    f.reload_registry();
    let npm_calls = install_gate(&mut f, |input, _| {
        allow_verdict(input, NestedDecision::Auto)
    });
    let npm_guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let npm_ticket = plugin_ticket(&npm_guard).unwrap();
    let npm_res = f
        .call_broker(
            &f.creator,
            HostCohort::Builtin,
            &npm_ticket,
            "install",
            json!({"target": dsh_dual_npm.to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(npm_res["application"], "failed", "{npm_res}");
    assert_eq!(npm_res["changed"], false);
    assert_eq!(npm_res["error"]["code"], "operation-error");
    assert!(
        npm_res["error"]["diagnostic"]
            .as_str()
            .unwrap()
            .contains("registry.npmjs.org"),
        "{npm_res}"
    );
    assert_eq!(npm_calls.lock().unwrap().len(), 0);
}
