//! Exercise native plan lowering and the real provider-binding spawn boundary.

use super::*;
use crate::client::CodewhaleClient;
use crate::fleet::exact::{ExactFleetWorkflow, StaticFleetRouter};
use crate::fleet::members::add_fleet_model;
use crate::fleet::store::{FleetFile, FleetScope, save_fleet, set_selected};
use crate::tools::subagent::new_shared_subagent_manager;
use codewhale_config::ProviderKind;
use codewhale_runtime::config::Config;
use codewhale_workflow::{FleetDocument, QualifiedFleetId};
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

const TARGET_MODEL: &str = "openai/gpt-4.1";
const TARGET_SELECTOR: &str = "openrouter/openai/gpt-4.1";

struct RouteFixture {
    config: Config,
    runtime: SubAgentRuntime,
    parent_calls: Arc<AtomicUsize>,
    target_calls: Arc<AtomicUsize>,
    target_bodies: Arc<Mutex<Vec<Value>>>,
}

impl RouteFixture {
    async fn new(workspace: &Path) -> Self {
        // WorkflowVm dispatches on its own thread. Keep saved routes in the
        // explicit workspace: the test-only personal-home resolver correctly
        // refuses another thread's environment guard.
        let fleet = FleetFile::new("Workflow fixture".into(), None).expect("empty saved Fleet");
        save_fleet(&fleet, FleetScope::Workspace, workspace).expect("save fixture Fleet");
        set_selected(&fleet.name, FleetScope::Workspace, workspace).expect("select fixture Fleet");
        let (parent, parent_calls, _, _) = tests::fake_chat_client_capturing("parent route").await;
        let (target, target_calls, target_bodies, _) =
            tests::fake_chat_client_capturing("frozen route result").await;
        let mut config = <Config as Default>::default().with_legacy_root(
            Some("fixture-key".into()),
            Some(parent.base_url().to_string()),
        );
        config
            .set_provider_api_key_override(
                &config.test_identity_for_kind(ProviderKind::Openrouter),
                Some("fixture-key".into()),
            )
            .unwrap();
        config
            .set_provider_base_url_override(
                &config.test_identity_for_kind(ProviderKind::Openrouter),
                Some(target.base_url().to_string()),
            )
            .unwrap();
        let client = CodewhaleClient::new(&config).expect("parent fixture client");
        let manager = new_shared_subagent_manager(workspace.to_path_buf(), 4);
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-pro".into(),
            ToolContext::new(workspace.to_path_buf()).with_state_namespace("session-test"),
            true,
            None,
            manager,
        )
        .with_api_config(config.clone());
        Self {
            config,
            runtime,
            parent_calls,
            target_calls,
            target_bodies,
        }
    }

    fn driver(&self, fleet: WorkflowFleetBinding) -> Arc<SubAgentWorkflowDriver> {
        let workspace = self.runtime.context.workspace.clone();
        let state = WorkflowWorkspaceState::open(&workspace);
        let run_id = format!("shortlist-{}", Uuid::new_v4());
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".into()),
                None,
                None,
                None,
            ),
        );
        SubAgentWorkflowDriver::new(
            run_id,
            "session-test".into(),
            self.runtime.manager.clone(),
            self.runtime.clone(),
            state,
            None,
            fleet,
            Vec::new(),
            workspace,
        )
    }

    fn assert_target_request(&self) {
        assert_eq!(
            self.parent_calls.load(Ordering::SeqCst),
            0,
            "parent route must not receive the child"
        );
        assert!(
            self.target_calls.load(Ordering::SeqCst) > 0,
            "the child must actually reach the selected provider"
        );
        let bodies = self.target_bodies.lock().expect("request bodies");
        assert!(!bodies.is_empty());
        for body in bodies.iter() {
            assert_eq!(
                body["model"], TARGET_MODEL,
                "provider wire model must match the saved route"
            );
        }
    }
}

/// The real VM has its own OS thread. Enroll it in this fixture's sealed test
/// environment before provider construction reads Config, and retain enrollment
/// while the real child tasks run on that same reactor. Otherwise Config's test
/// reader blocks on the environment lock held by the test awaiting the VM.
struct ScopedWorkflowDriver {
    inner: Arc<SubAgentWorkflowDriver>,
    ticket: crate::test_support::EnvScopeTicket,
    membership: Mutex<Option<crate::test_support::EnvScopeMembership>>,
}

#[async_trait]
impl WorkflowDriver for ScopedWorkflowDriver {
    async fn spawn_task(&self, request: TaskRequest) -> Result<SpawnedTask, DriverError> {
        {
            let mut membership = self.membership.lock().expect("VM environment membership");
            if membership.is_none() {
                *membership = Some(
                    crate::test_support::join_env_scope(Some(self.ticket))
                        .expect("the originating test still owns its environment"),
                );
            }
        }
        self.inner.spawn_task(request).await
    }

    fn cancel_all(&self) {
        self.inner.cancel_all();
    }

    fn budget(&self) -> BudgetSnapshot {
        self.inner.budget()
    }

    fn progress(&self, event: ProgressEvent) {
        self.inner.progress(event);
    }
}

async fn run_script(source: &str, driver: Arc<SubAgentWorkflowDriver>) -> Result<Value, String> {
    let scoped = Arc::new(ScopedWorkflowDriver {
        inner: driver.clone(),
        ticket: crate::test_support::env_scope_ticket().expect("fixture owns test environment"),
        membership: Mutex::new(None),
    });
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        WorkflowVm::new().run_script(source, json!({}), scoped),
    )
    .await;
    driver.cancel_all();
    result
        .expect("workflow fixture must settle within ten seconds")
        .map_err(|error| error.to_string())
}

fn native_script(child: Value) -> String {
    let spec = structured_plan_to_workflow_spec(&json!({
        "goal": "review the route fixture", "risk": "read_only", "children": [child],
    }))
    .expect("valid native plan");
    lower_declarative_workflow_to_imperative_js(&spec).expect("lower native plan")
}

fn select_role(workspace: &Path, model: &str, provider: &str) {
    let fleet = FleetFile::parse(&format!(
        "schema = 'fleet'\nschema_revision = 2\nname = 'Selected roster'\n\
         [[members]]\nid = 'auditor'\nrole = 'reviewer'\nprovider = '{provider}'\n\
         model = '{model}'\nreasoning = 'off'\n"
    ))
    .expect("saved roster");
    save_fleet(&fleet, FleetScope::Workspace, workspace).expect("save selected roster");
    set_selected(&fleet.name, FleetScope::Workspace, workspace).expect("select roster");
}

fn exact_document(reasoning: &str) -> FleetDocument {
    let router = if reasoning == "auto" {
        "reasoning_router = 'fixture-router'\n"
    } else {
        ""
    };
    FleetDocument::parse(&format!(
        "name = 'frozen-audit'\nschema = 'exact'\n{router}\
         [[members]]\nid = 'auditor'\nrole = 'reviewer'\nprovider = 'openrouter'\n\
         model = '{TARGET_MODEL}'\nreasoning = '{reasoning}'\n"
    ))
    .expect("exact Fleet document")
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_shortlisted_model_reaches_its_configured_provider_request() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    add_fleet_model(root.path(), "openrouter", TARGET_MODEL, &[]).unwrap();
    let source = native_script(json!({
        "prompt": "read-only route check", "type": "reviewer", "model": TARGET_SELECTOR,
    }));
    run_script(&source, fixture.driver(WorkflowFleetBinding::None))
        .await
        .expect("shortlisted native child runs");
    fixture.assert_target_request();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_non_shortlisted_model_is_rejected_without_a_provider_call() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    add_fleet_model(root.path(), "openrouter", TARGET_MODEL, &[]).unwrap();
    let source = native_script(json!({
        "prompt": "read-only route check", "type": "reviewer", "model": "openrouter/outside-fixture",
    }));
    let driver = fixture.driver(WorkflowFleetBinding::None);
    let result = run_script(&source, driver.clone())
        .await
        .expect("rejected children settle without aborting independent work");
    assert_eq!(result, json!({"plan-child-1": null}));
    let runs = driver.state.runs.lock().expect("run ledger");
    let record = runs.get(&driver.run_id).expect("run receipt");
    assert_eq!(record.dispatch_failure_count, 1);
    assert!(
        record.dispatch_failures[0]
            .message
            .contains("outside the selected Fleet")
    );
    assert_eq!(driver.dead_fanout_count(), 1);
    assert_eq!(fixture.parent_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.target_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_role_without_model_uses_the_saved_provider_and_thinking() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    select_role(root.path(), TARGET_MODEL, "openrouter");
    let source = native_script(json!({"prompt": "read-only route check", "profile": "auditor"}));
    let driver = fixture.driver(WorkflowFleetBinding::None);
    run_script(&source, driver.clone())
        .await
        .expect("saved role runs");
    fixture.assert_target_request();
    let runs = driver.state.runs.lock().expect("runs");
    let run = runs.get(&driver.run_id).expect("run record");
    let started = run
        .events
        .iter()
        .find(|event| event.event_type() == "task_started")
        .expect("task receipt");
    let WorkflowUiEventKind::TaskStarted(started) = &started.kind else {
        unreachable!()
    };
    assert_eq!(started.resolved_profile.as_deref(), Some("auditor"));
    assert_eq!(started.resolved_provider, "openrouter");
    assert_eq!(started.resolved_model, TARGET_MODEL);
    assert_eq!(started.effective_reasoning.as_deref(), Some("off"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exact_fleet_frozen_route_survives_a_conflicting_selected_roster() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    // Capture first. Later selected-Pod edits must affect neither this member
    // nor the preflighted provider endpoint, even when the member id collides.
    let operation = ExactFleetWorkflow::capture(
        &exact_document("off"),
        QualifiedFleetId {
            name: "frozen-audit".into(),
            origin: "workspace".into(),
        },
        "2026-09-13T00:00:00Z",
        Some(&fixture.config),
        &[],
    )
    .expect("preflight exact route against loopback provider");
    select_role(root.path(), "deepseek-v4-pro", "deepseek");
    let source =
        native_script(json!({"prompt": "read-only frozen route check", "profile": "auditor"}));
    run_script(
        &source,
        fixture.driver(WorkflowFleetBinding::Exact(Arc::new(operation))),
    )
    .await
    .expect("frozen exact child runs independently of selected Fleet");
    fixture.assert_target_request();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exact_fleet_model_override_is_rejected_before_router_or_provider() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    let router = StaticFleetRouter::new(r#"{"reasoning":"high"}"#);
    let operation = ExactFleetWorkflow::for_tests(
        &exact_document("auto"),
        QualifiedFleetId {
            name: "frozen-audit".into(),
            origin: "workspace".into(),
        },
        Some(router.clone()),
    );
    let error = run_script(
        "return await task({description:'read-only check', profile:'auditor', writeAuthority:'read_only', model:'openrouter/outside-fixture'});",
        fixture.driver(WorkflowFleetBinding::Exact(Arc::new(operation))),
    ).await.expect_err("exact Fleet refuses task route overrides");
    assert!(
        error.contains("task option `model` is not allowed"),
        "{error}"
    );
    assert!(
        router.seen.lock().unwrap().is_empty(),
        "rejection must precede reasoning spend"
    );
    assert_eq!(fixture.parent_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.target_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exact_fleet_changed_provider_endpoint_is_rejected_before_dispatch() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let mut fixture = RouteFixture::new(root.path()).await;
    let operation = ExactFleetWorkflow::capture(
        &exact_document("off"),
        QualifiedFleetId {
            name: "frozen-audit".into(),
            origin: "workspace".into(),
        },
        "2026-09-13T00:00:00Z",
        Some(&fixture.config),
        &[],
    )
    .expect("capture exact route");
    let mut changed = fixture.config.clone();
    changed
        .set_provider_base_url_override(
            &changed.test_identity_for_kind(ProviderKind::Openrouter),
            Some(fixture.runtime.client.base_url().to_string()),
        )
        .unwrap();
    fixture.runtime.api_config = Some(Arc::new(changed));
    let error = run_script(
        "return await task({description:'read-only check', profile:'auditor', writeAuthority:'read_only'});",
        fixture.driver(WorkflowFleetBinding::Exact(Arc::new(operation))),
    )
    .await
    .expect_err("changed endpoint cannot inherit the frozen receipt");
    assert!(error.contains("exact Fleet route changed"), "{error}");
    assert_eq!(fixture.parent_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.target_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_exact_fleet_builder_keeps_the_plan_read_only_ceiling() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    let document = FleetDocument::parse(&format!(
        "name = 'frozen-builder'\nschema = 'exact'\n[[members]]\n\
         id = 'builder-one'\nrole = 'builder'\nprovider = 'openrouter'\n\
         model = '{TARGET_MODEL}'\nreasoning = 'off'\n"
    ))
    .unwrap();
    let operation = ExactFleetWorkflow::capture(
        &document,
        QualifiedFleetId {
            name: "frozen-builder".into(),
            origin: "workspace".into(),
        },
        "2026-09-13T00:00:00Z",
        Some(&fixture.config),
        &[],
    )
    .expect("capture exact builder");
    let source =
        native_script(json!({"prompt": "inspect without changing files", "role": "builder"}));
    run_script(
        &source,
        fixture.driver(WorkflowFleetBinding::Exact(Arc::new(operation))),
    )
    .await
    .expect("a native plan may narrow an exact builder");
    fixture.assert_target_request();
    let manager = fixture.runtime.manager.read().await;
    let records = manager.list_worker_records();
    assert_eq!(records.len(), 1);
    let profile = &records[0].spec.runtime_profile;
    assert!(
        !profile.permissions.write,
        "the authored read_only mode must remain executable policy"
    );
    assert_eq!(
        profile.shell,
        codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly
    );
    assert_eq!(
        profile.tools,
        codewhale_agent_contract::worker_profile::ToolScope::Inherit
    );
    assert_eq!(
        records[0].spec.child_route.as_ref().unwrap().provider_id,
        "openrouter"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn exact_fleet_task_cannot_widen_a_read_only_role_before_router_or_provider() {
    let _retry = tests::workflow_test_retry_guard();
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
    let fixture = RouteFixture::new(root.path()).await;
    let router = StaticFleetRouter::new(r#"{"reasoning":"high"}"#);
    let operation = ExactFleetWorkflow::for_tests(
        &exact_document("auto"),
        QualifiedFleetId {
            name: "frozen-audit".into(),
            origin: "workspace".into(),
        },
        Some(router.clone()),
    );
    let error = run_script(
        "return await task({description:'attempt widening', profile:'auditor', writeAuthority:'workspace_write', writeRoots:['src']});",
        fixture.driver(WorkflowFleetBinding::Exact(Arc::new(operation))),
    ).await.expect_err("task cannot widen the Runtime role");
    assert!(error.contains("cannot request write authority"), "{error}");
    assert!(router.seen.lock().unwrap().is_empty());
    assert_eq!(fixture.parent_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.target_calls.load(Ordering::SeqCst), 0);
}
