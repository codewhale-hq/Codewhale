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
            .with_origin_turn_id("plugins-turn");
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
    let token = registry
        .host_service_review_for(selector, &approver)
        .map(|review| review.token);
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
async fn the_four_changing_methods_still_answer_not_available() {
    let mut f = PluginsFixture::new();
    f.reload_registry();
    let guard = f.begin(&f.creator, HostCohort::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();

    for method in ["install", "remove", "enable", "disable"] {
        let err = f
            .call_broker(
                &f.creator,
                HostCohort::Builtin,
                &ticket,
                method,
                json!({"plugin": "alpha"}),
            )
            .await;
        assert_eq!(
            refusal(&err),
            (error_code::NOT_AVAILABLE, "not_available".into()),
            "{method}"
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

    let guard = f.begin(&f.creator, HostTier::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let answer = f
        .call_broker(&f.creator, HostTier::Builtin, &ticket, "tools", json!({}))
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

    let guard = f.begin(&f.creator, HostTier::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let answer = f
        .call_broker(&f.creator, HostTier::Builtin, &ticket, "tools", json!({}))
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

    let guard = f.begin(&f.plugin_owner, HostTier::Plugin, &f.context);
    assert!(plugin_ticket(&guard).is_none());

    let mut sub_context = f.context.clone();
    sub_context.execution.owner_agent_id = Some("sub-agent-1".into());
    let sub_guard = f.begin(&f.creator, HostTier::Builtin, &sub_context);
    assert!(plugin_ticket(&sub_guard).is_none());

    let creator_guard = f.begin(&f.creator, HostTier::Builtin, &f.context);
    let creator_ticket = plugin_ticket(&creator_guard).unwrap();
    let plugin_err = f
        .call_broker(
            &f.plugin_owner,
            HostTier::Plugin,
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
            HostTier::Builtin,
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

    let guard = f.begin(&f.creator, HostTier::Builtin, &f.context);
    let ticket = plugin_ticket(&guard).unwrap();
    let list = f
        .call_broker(&f.creator, HostTier::Builtin, &ticket, "list", json!({}))
        .await
        .unwrap();
    let status = f
        .call_broker(&f.creator, HostTier::Builtin, &ticket, "status", json!({}))
        .await
        .unwrap();
    let list_str = serde_json::to_string(&list).unwrap();
    let status_str = serde_json::to_string(&status).unwrap();
    assert!(!list_str.contains(secret));
    assert!(!status_str.contains(secret));
}
