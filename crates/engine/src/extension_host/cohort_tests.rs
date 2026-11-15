use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::protocol::DshService;
use super::registry::OwnerState;
use super::supervisor::HostSandbox;
use super::test_support::{
    fake_authority, fixtures_dir, host_tool, node_for_tests, stub_builtin_commands,
};
use super::tier::{HostCohort, HostTier, PluginCohort};
use super::{ExtensionHostManager, ExtensionHostOptions, HostSlot};
use crate::command_catalog::CatalogGuard;
use crate::tools::spec::ToolContext;
use codewhale_config::OwnerRef;
use codewhale_config::extension_host::ExtensionHostRuntime;
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::host_services::{
    AgentApproval, AgentPosture, PersonVia, PluginApprover, ReviewApproval,
};
use codewhale_plugins::install::{
    DEFAULT_MAX_SIZE_BYTES, PluginInstallOutcome, PluginInstallSource, install,
};

const NODE: ExtensionHostRuntime = ExtensionHostRuntime::Node;

fn fast_supervision() -> super::SupervisionOptions {
    super::SupervisionOptions {
        heartbeat_interval: Duration::from_millis(50),
        ping_timeout: Duration::from_millis(200),
        hang_timeout: Duration::from_millis(500),
        restart_backoff: Duration::from_millis(10),
        crash_window: Duration::from_secs(10),
        crash_limit: 3,
        start_retry_cooldown: Duration::from_millis(10),
        dirty_window: Duration::from_secs(10),
        dirty_limit: 2,
        memory_cap: super::supervisor::HOST_MEMORY_CAP,
        tool_call_deadline: Duration::from_secs(3),
        command_run_deadline: Duration::from_secs(3),
    }
}

async fn wait_host(manager: &ExtensionHostManager, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("host wait timed out: status={:?}", manager.status());
    });
}

fn cohort_pid(manager: &ExtensionHostManager, cohort: &HostCohort) -> Option<u32> {
    let rt = manager.shared.cohort_runtime(cohort)?;
    let slot = rt.host.lock().expect("host lock");
    match &*slot {
        HostSlot::Ready(host) => host.pid,
        _ => None,
    }
}

struct CohortTestFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    user: PathBuf,
    home: PathBuf,
    config: DiscoveryConfig,
    _commands: CatalogGuard,
}

impl CohortTestFixture {
    async fn new(names: &[&str]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        let user = temp.path().join("user");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        for name in names {
            let outcome = install(
                PluginInstallSource::LocalPath(fixtures_dir().join(name)),
                &user,
                DEFAULT_MAX_SIZE_BYTES,
                &codewhale_net::network_policy::NetworkPolicy::default(),
                false,
                &|_| None,
            )
            .await
            .unwrap_or_else(|error| panic!("install {name}: {error:#}"));
            assert!(
                matches!(outcome, PluginInstallOutcome::Installed(ref installed) if installed.name == *name),
                "install {name}: {outcome:?}"
            );
        }

        let config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: user.clone(),
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: Vec::new(),
            state_path: temp.path().join("state/plugin-state.json"),
        };

        Self {
            _temp: temp,
            workspace,
            user,
            home,
            config,
            _commands: stub_builtin_commands(),
        }
    }

    async fn subproject(&self, name: &str, names: &[&str]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join(name);
        std::fs::create_dir_all(&workspace).unwrap();
        for plugin in names {
            let outcome = install(
                PluginInstallSource::LocalPath(fixtures_dir().join(plugin)),
                &self.user,
                DEFAULT_MAX_SIZE_BYTES,
                &codewhale_net::network_policy::NetworkPolicy::default(),
                false,
                &|_| None,
            )
            .await
            .unwrap_or_else(|error| panic!("install {plugin}: {error:#}"));
            assert!(
                matches!(outcome, PluginInstallOutcome::Installed(ref installed) if installed.name == *plugin),
                "install {plugin}: {outcome:?}"
            );
        }
        let config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: self.user.clone(),
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: Vec::new(),
            state_path: self.config.state_path.clone(),
        };
        Self {
            _temp: temp,
            workspace,
            user: self.user.clone(),
            home: self.home.clone(),
            config,
            _commands: stub_builtin_commands(),
        }
    }

    fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn registry(&self) -> Arc<PluginRegistry> {
        Arc::new(discover_with_config(&self.config))
    }

    fn plugin_id(&self, name: &str) -> String {
        self.registry().get(name).unwrap().id.as_str().to_string()
    }

    fn review_by_person(&self, name: &str) {
        let mut registry = discover_with_config(&self.config);
        let shown = registry.host_service_review(name).map(|r| r.token);
        registry
            .trust_reviewed(
                name,
                ReviewApproval {
                    approved_by: PluginApprover::person(PersonVia::Terminal),
                    host_services: shown,
                },
            )
            .unwrap();
        registry.enable(name).unwrap();
    }

    fn review_by_agent(&self, name: &str) {
        let mut registry = discover_with_config(&self.config);
        let approver = PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::FullAccess,
            workspace: self.workspace.to_string_lossy().into_owned(),
            thread_id: Some("thread".into()),
            turn_id: Some("turn".into()),
            tool_call_id: Some("call".into()),
        });
        let shown = registry
            .host_service_review_for(name, &approver)
            .map(|r| r.token);
        registry
            .trust_reviewed(
                name,
                ReviewApproval {
                    approved_by: approver.clone(),
                    host_services: shown,
                },
            )
            .unwrap();
        registry.enable_by(name, approver).unwrap();
    }

    fn review_by_marked_shell(&self, name: &str) {
        let mut registry = discover_with_config(&self.config);
        let approver = PluginApprover::command_line(Some(&self.workspace));
        let shown = registry
            .host_service_review_for(name, &approver)
            .map(|r| r.token);
        registry
            .trust_reviewed(
                name,
                ReviewApproval {
                    approved_by: approver.clone(),
                    host_services: shown,
                },
            )
            .unwrap();
        registry.enable_by(name, approver).unwrap();
    }

    fn review_by_unmarked_shell(&self, name: &str) {
        let mut registry = discover_with_config(&self.config);
        let approver = PluginApprover::command_line(None);
        registry
            .trust_reviewed(
                name,
                ReviewApproval {
                    approved_by: approver.clone(),
                    host_services: None,
                },
            )
            .unwrap();
        registry.enable_by(name, approver).unwrap();
    }

    fn review_by_auto_review(&self, name: &str, workspace: &Path) {
        let mut config = self.config.clone();
        config.workspace = workspace.to_path_buf();
        config.workspace_plugins_dir = workspace.join(".codewhale/plugins");
        let mut registry = discover_with_config(&config);
        let approver = PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::AutoReview,
            workspace: workspace.to_string_lossy().into_owned(),
            thread_id: Some("thread".into()),
            turn_id: Some("turn".into()),
            tool_call_id: Some("call".into()),
        });
        let shown = registry
            .host_service_review_for(name, &approver)
            .map(|r| r.token);
        registry
            .trust_reviewed(
                name,
                ReviewApproval {
                    approved_by: approver.clone(),
                    host_services: shown,
                },
            )
            .unwrap();
        registry.enable_by(name, approver).unwrap();
    }

    fn manager(&self, node: PathBuf) -> Arc<ExtensionHostManager> {
        Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: NODE,
            node_override: Some(node),
            root: Some(self.home.clone()),
            ..Default::default()
        }))
    }

    fn manager_with_supervision(
        &self,
        node: PathBuf,
        supervision: super::SupervisionOptions,
    ) -> Arc<ExtensionHostManager> {
        Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: NODE,
            node_override: Some(node),
            root: Some(self.home.clone()),
            supervision,
            ..Default::default()
        }))
    }
}

#[tokio::test]
async fn an_agent_approved_owner_never_loads_into_the_reviewed_host() {
    let Some(node) = node_for_tests("an_agent_approved_owner_never_loads_into_the_reviewed_host")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = CohortTestFixture::new(&["two-entries", "child-persona"]).await;

    fixture.review_by_person("two-entries");
    fixture.review_by_agent("child-persona");

    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    let pid_reviewed =
        cohort_pid(&manager, &HostCohort::reviewed()).expect("reviewed host must be running");
    let cohort_agent = HostCohort::agent(fixture.workspace());
    let pid_agent = cohort_pid(&manager, &cohort_agent).expect("agent host must be running");
    assert_ne!(pid_reviewed, pid_agent);

    {
        let reg = manager.shared.registry.lock().unwrap();
        let child_id = fixture.plugin_id("child-persona");
        let entry = reg.owner(&child_id).expect("agent plugin must be present");
        assert_eq!(entry.cohort, cohort_agent);
    }

    let tool = host_tool(&engine, fixture.workspace(), "two_first");
    let result = tool
        .execute(
            json!({}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
        )
        .await;
    assert!(result.is_ok());
}

#[test]
fn a_ticket_minted_for_one_cohort_is_refused_by_another() {
    use super::ticket::{Grant, Presented, Refusal, TicketKind, TicketTable};
    let tickets = TicketTable::default();
    let cohort1 = HostCohort::reviewed();
    let cohort2 = HostCohort::agent("/workspace/test-proj");

    let owner = OwnerRef {
        plugin_id: "test-plugin".into(),
        generation: 1,
        owner_token: "tok".into(),
    };

    let target = json!({"path": "/some/path"});
    let grant = Grant {
        kind: TicketKind::Invocation,
        cohort: cohort1.clone(),
        host_generation: 1,
        owner: owner.clone(),
        method: "proc/read",
        target: target.clone(),
        ttl: Duration::from_secs(60),
        uses: 1,
    };
    let ticket = tickets.mint(grant);

    let presented_wrong_cohort = Presented {
        ticket: ticket.expose(),
        kind: TicketKind::Invocation,
        cohort: cohort2.clone(),
        host_generation: 1,
        owner: &owner,
        method: "proc/read",
        target: Some(&target),
    };

    let result = tickets.redeem(&presented_wrong_cohort);
    assert_eq!(result.unwrap_err().reason, Refusal::WrongCohort);

    let presented_right_cohort = Presented {
        ticket: ticket.expose(),
        kind: TicketKind::Invocation,
        cohort: cohort1,
        host_generation: 1,
        owner: &owner,
        method: "proc/read",
        target: Some(&target),
    };
    assert!(tickets.redeem(&presented_right_cohort).is_ok());
}

#[tokio::test]
async fn two_projects_agent_plugins_do_not_share_a_host() {
    let Some(node) = node_for_tests("two_projects_agent_plugins_do_not_share_a_host") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture1 = CohortTestFixture::new(&["two-entries", "child-persona"]).await;
    let fixture2 = fixture1.subproject("project2", &["plugin-context"]).await;

    fixture1.review_by_agent("two-entries");
    fixture1.review_by_person("child-persona");
    fixture2.review_by_agent("plugin-context");

    let manager = fixture1.manager(node);
    let engine1 = manager.attach(fixture1.registry());
    let engine2 = manager.attach(fixture2.registry());

    engine1.sync().await.unwrap();
    engine2.sync().await.unwrap();

    let cohort1 = HostCohort::agent(fixture1.workspace());
    let cohort2 = HostCohort::agent(fixture2.workspace());
    let cohort_reviewed = HostCohort::reviewed();

    let pid1 = cohort_pid(&manager, &cohort1).expect("host for project 1 must be running");
    let pid2 = cohort_pid(&manager, &cohort2).expect("host for project 2 must be running");
    let pid_reviewed =
        cohort_pid(&manager, &cohort_reviewed).expect("reviewed host must be running");
    assert_ne!(pid1, pid2);
    assert_ne!(pid1, pid_reviewed);
    assert_ne!(pid2, pid_reviewed);

    let tool1 = host_tool(&engine1, fixture1.workspace(), "two_first");
    let outcome1 = tool1
        .execute(
            json!({}),
            &ToolContext::new(fixture1.workspace()).with_plugin_registry(engine1.plugin_view()),
        )
        .await;
    assert!(outcome1.is_ok());

    let tool2 = host_tool(&engine2, fixture2.workspace(), "ctx_probe");
    let outcome2 = tool2
        .execute(
            json!({}),
            &ToolContext::new(fixture2.workspace()).with_plugin_registry(engine2.plugin_view()),
        )
        .await;
    assert!(outcome2.is_ok());

    let mut tools1 = crate::tools::ToolRegistry::new(ToolContext::new(fixture1.workspace()));
    engine1.install_tools(&mut tools1);
    assert!(tools1.get("two_first").is_some());
    assert!(tools1.get("ctx_probe").is_none());

    let mut tools2 = crate::tools::ToolRegistry::new(ToolContext::new(fixture2.workspace()));
    engine2.install_tools(&mut tools2);
    assert!(tools2.get("ctx_probe").is_some());
    assert!(tools2.get("two_first").is_none());

    let agent_cohorts = manager.shared.agent_cohorts.lock().unwrap();
    assert_eq!(agent_cohorts.len(), 2);
}

#[tokio::test]
async fn a_person_review_moves_the_owner_and_its_data() {
    let Some(node) = node_for_tests("a_person_review_moves_the_owner_and_its_data") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = CohortTestFixture::new(&["two-entries", "plugin-context", "crash-tool"]).await;
    fixture.review_by_person("two-entries");
    fixture.review_by_agent("plugin-context");
    fixture.review_by_agent("crash-tool");

    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    let pid_reviewed_before =
        cohort_pid(&manager, &HostCohort::reviewed()).expect("reviewed host pre-started");
    let agent_cohort = HostCohort::agent(fixture.workspace());
    let pid_agent_before = cohort_pid(&manager, &agent_cohort).expect("agent host must be running");
    assert_ne!(pid_reviewed_before, pid_agent_before);

    let context = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let note_tool = host_tool(&engine, fixture.workspace(), "ctx_note");
    let write_res = note_tool
        .execute(json!({"text": "written by agent cohort"}), &context)
        .await
        .unwrap();
    assert!(write_res.success);
    let payload: serde_json::Value = serde_json::from_str(&write_res.content).unwrap();
    assert_eq!(payload["text"], "written by agent cohort");

    let plugin_id = fixture.plugin_id("plugin-context");
    let agent_data = super::supervisor::owner_data_dir(
        &fixture.home,
        &agent_cohort,
        &plugin_id,
        "plugin-context",
    );
    assert!(agent_data.join("note.txt").exists());

    fixture.review_by_person("plugin-context");
    engine.set_plugins(fixture.registry());
    engine.sync().await.ok();

    let reviewed_data =
        super::supervisor::plugin_data_dir(&fixture.home, &plugin_id, "plugin-context");
    assert!(
        !reviewed_data.exists(),
        "migration deferred while agent host live"
    );
    assert!(
        agent_data.join("note.txt").exists(),
        "agent data intact while deferred"
    );

    let mut reg = (*fixture.registry()).clone();
    reg.disable("crash-tool").unwrap();
    engine.set_plugins(Arc::new(reg));
    engine.sync().await.ok();

    wait_host(&manager, || reviewed_data.exists() && !agent_data.exists()).await;

    let content = std::fs::read_to_string(reviewed_data.join("note.txt")).unwrap();
    assert_eq!(content, "written by agent cohort");

    engine.sync().await.ok();
    assert_eq!(
        cohort_pid(&manager, &HostCohort::reviewed()),
        Some(pid_reviewed_before),
        "reviewed host remained running across migration"
    );
    let probe_tool = host_tool(&engine, fixture.workspace(), "ctx_probe");
    let probe_context =
        ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let probe_res = probe_tool.execute(json!({}), &probe_context).await.unwrap();
    assert!(probe_res.success);
    let probe_payload: serde_json::Value = serde_json::from_str(&probe_res.content).unwrap();
    assert_eq!(
        probe_payload["dataDir"].as_str(),
        Some(reviewed_data.to_string_lossy().as_ref())
    );

    let reviewed_note_tool = host_tool(&engine, fixture.workspace(), "ctx_note");
    let second_write = reviewed_note_tool
        .execute(json!({"text": "written in reviewed host"}), &probe_context)
        .await
        .unwrap();
    assert!(second_write.success);
    assert_eq!(
        std::fs::read_to_string(reviewed_data.join("note.txt")).unwrap(),
        "written in reviewed host"
    );
}

#[tokio::test]
async fn a_crash_loop_in_an_agent_cohort_leaves_the_reviewed_host_running() {
    let Some(node) =
        node_for_tests("a_crash_loop_in_an_agent_cohort_leaves_the_reviewed_host_running")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = CohortTestFixture::new(&["two-entries", "crash-tool"]).await;
    fixture.review_by_person("two-entries");
    fixture.review_by_agent("crash-tool");

    let manager = fixture.manager_with_supervision(node, fast_supervision());
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    let pid_reviewed =
        cohort_pid(&manager, &HostCohort::reviewed()).expect("reviewed host must be running");
    let agent_cohort = HostCohort::agent(fixture.workspace());
    let pid_agent = cohort_pid(&manager, &agent_cohort).expect("agent host must be running");
    assert_ne!(pid_reviewed, pid_agent);

    let mut prev_pid = pid_agent;
    let mut dead_tool = None;
    for crash in 0..3 {
        let tool = host_tool(&engine, fixture.workspace(), "crash_probe");
        let outcome = tool
            .execute(
                json!({}),
                &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
            )
            .await;
        assert!(outcome.is_err());
        dead_tool = Some(tool);
        if crash < 2 {
            wait_host(&manager, || {
                cohort_pid(&manager, &agent_cohort).is_some_and(|pid| pid != prev_pid)
                    && manager.live_tool_names().contains(&"crash_probe".into())
            })
            .await;
            prev_pid = cohort_pid(&manager, &agent_cohort).expect("restarted agent host pid");
        }
    }

    wait_host(&manager, || {
        manager
            .shared
            .cohort_runtime(&agent_cohort)
            .is_some_and(|rt| {
                rt.host
                    .lock()
                    .map(|slot| matches!(&*slot, HostSlot::Failed { .. }))
                    .unwrap_or(false)
            })
    })
    .await;

    assert!(
        !super::test_support::installed(&engine, fixture.workspace())
            .contains(&"crash_probe".to_string())
    );
    let dead_outcome = dead_tool
        .expect("crash_probe ran")
        .execute(
            json!({}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
        )
        .await;
    assert!(dead_outcome.is_err());

    let pid_reviewed_after =
        cohort_pid(&manager, &HostCohort::reviewed()).expect("reviewed host must still be running");
    assert_eq!(pid_reviewed, pid_reviewed_after);

    let tool2 = host_tool(&engine, fixture.workspace(), "two_first");
    let outcome2 = tool2
        .execute(
            json!({}),
            &ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()),
        )
        .await;
    assert!(outcome2.is_ok());
}

#[tokio::test]
async fn a_departed_owner_retires_its_agent_host_when_idle() {
    let Some(node) = node_for_tests("a_departed_owner_retires_its_agent_host_when_idle") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = CohortTestFixture::new(&["two-entries", "child-persona"]).await;
    fixture.review_by_agent("two-entries");
    fixture.review_by_agent("child-persona");

    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    let agent_cohort = HostCohort::agent(fixture.workspace());
    let pid1 = cohort_pid(&manager, &agent_cohort).expect("agent host running");
    let gen1 = {
        let rt = manager.shared.cohort_runtime(&agent_cohort).unwrap();
        rt.host_generation.load(std::sync::atomic::Ordering::SeqCst)
    };

    let mut reg = discover_with_config(&fixture.config);
    reg.disable("two-entries").unwrap();
    engine.set_plugins(Arc::new(reg));
    engine.sync().await.unwrap();

    assert_eq!(
        cohort_pid(&manager, &agent_cohort),
        Some(pid1),
        "disabling 1st owner does not stop agent host while 2nd owner remains"
    );

    let mut reg2 = discover_with_config(&fixture.config);
    reg2.disable("two-entries").unwrap();
    reg2.disable("child-persona").unwrap();
    engine.set_plugins(Arc::new(reg2));
    engine.sync().await.unwrap();

    wait_host(&manager, || {
        let rt = manager.shared.cohort_runtime(&agent_cohort);
        rt.as_ref().map_or(true, |rt| {
            rt.host
                .lock()
                .map(|slot| matches!(&*slot, HostSlot::Idle))
                .unwrap_or(false)
        })
    })
    .await;

    let plugin_id = fixture.plugin_id("child-persona");
    let agent_data = super::supervisor::owner_data_dir(
        &fixture.home,
        &agent_cohort,
        &plugin_id,
        "child-persona",
    );
    assert!(agent_data.parent().unwrap().exists());

    fixture.review_by_agent("two-entries");
    engine.set_plugins(fixture.registry());
    engine.sync().await.unwrap();

    let pid2 = cohort_pid(&manager, &agent_cohort).expect("agent host restarted");
    assert_ne!(pid1, pid2);
    let gen2 = {
        let rt = manager.shared.cohort_runtime(&agent_cohort).unwrap();
        rt.host_generation.load(std::sync::atomic::Ordering::SeqCst)
    };
    assert!(gen2 > gen1);
}

#[test]
fn granting_a_host_service_to_an_agent_cohort_grants_it_to_every_plugin_in_that_cohort_and_no_other_cohort()
 {
    let mut reg = super::registry::OwnerRegistry::default();

    let cohort1 = HostCohort::agent("/workspace/alpha");
    let cohort2 = HostCohort::agent("/workspace/beta");

    let auth1 = fake_authority("plugin-a");
    let auth2 = fake_authority("plugin-b");
    let auth3 = fake_authority("plugin-c");

    let owner1 = reg
        .begin_owner(
            cohort1.clone(),
            "plugin-a",
            "Plugin A",
            Some(auth1),
            "hash-a",
        )
        .unwrap();
    let owner2 = reg
        .begin_owner(
            cohort1.clone(),
            "plugin-b",
            "Plugin B",
            Some(auth2),
            "hash-b",
        )
        .unwrap();
    let owner3 = reg
        .begin_owner(
            cohort2.clone(),
            "plugin-c",
            "Plugin C",
            Some(auth3),
            "hash-c",
        )
        .unwrap();

    reg.set_host_services(&owner1, vec![DshService::Network]);
    reg.set_host_services(&owner2, vec![DshService::Network]);
    reg.set_host_services(&owner3, vec![]);

    assert!(reg.host_service_granted(&owner1, DshService::Network));
    assert!(reg.host_service_granted(&owner2, DshService::Network));
    assert!(!reg.host_service_granted(&owner3, DshService::Network));

    reg.set_host_services(&owner2, vec![]);
    assert!(!reg.host_service_granted(&owner1, DshService::Network));
    assert!(!reg.host_service_granted(&owner2, DshService::Network));

    reg.revoke_owner(&owner2.plugin_id);
    assert!(!reg.host_service_granted(&owner1, DshService::Network));
    reg.host_process_started(cohort1);
    assert!(reg.host_service_granted(&owner1, DshService::Network));
}

#[tokio::test]
async fn an_agent_cohort_host_cannot_read_or_write_another_cohorts_data() {
    let home = Path::new("/tmp/cw-cohort-home");
    let cohort_a = HostCohort::agent("/proj/alpha");
    let cohort_b = HostCohort::agent("/proj/beta");
    let dir_a = super::supervisor::host_cohort_data_dir(home, &cohort_a).unwrap();
    let dir_b = super::supervisor::host_cohort_data_dir(home, &cohort_b).unwrap();
    let reviewed_dir =
        super::supervisor::host_cohort_data_dir(home, HostCohort::reviewed()).unwrap();
    let builtin_dir = super::supervisor::host_cohort_data_dir(home, &HostCohort::Builtin).unwrap();

    assert_ne!(dir_a, dir_b);
    assert_ne!(dir_a, reviewed_dir);
    assert_ne!(dir_b, reviewed_dir);
    assert_ne!(dir_a, builtin_dir);
    assert!(dir_a.to_string_lossy().contains("data-agent"));
    assert!(dir_b.to_string_lossy().contains("data-agent"));
    assert!(reviewed_dir.to_string_lossy().ends_with("data"));

    let temp_home = tempfile::tempdir().unwrap();
    let (denied_paths, _) =
        super::supervisor::host_denied_read_paths(&cohort_a, temp_home.path(), false);
    let reviewed_path = super::supervisor::tier_data_dir(temp_home.path(), HostTier::Plugin);
    let builtin_path = super::supervisor::tier_data_dir(temp_home.path(), HostTier::Builtin);
    assert!(
        denied_paths.contains(&reviewed_path),
        "agent cohort must deny reviewed data path"
    );
    assert!(
        denied_paths.contains(&builtin_path),
        "agent cohort must deny builtin data path"
    );

    let Some(node) =
        node_for_tests("an_agent_cohort_host_cannot_read_or_write_another_cohorts_data")
    else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = CohortTestFixture::new(&["secret-probe", "two-entries"]).await;
    fixture.review_by_agent("secret-probe");
    fixture.review_by_agent("two-entries");

    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    let plugin_id = fixture.plugin_id("two-entries");
    let agent_cohort = HostCohort::agent(fixture.workspace());
    let agent_data =
        super::supervisor::owner_data_dir(&fixture.home, &agent_cohort, &plugin_id, "two-entries");
    std::fs::create_dir_all(&agent_data).unwrap();
    std::fs::write(agent_data.join("agent_file.txt"), "agent data").unwrap();

    let dest_dir = super::supervisor::plugin_data_dir(&fixture.home, &plugin_id, "two-entries");
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::write(dest_dir.join("existing.txt"), "pre-existing reviewed data").unwrap();

    let is_sandboxed = manager
        .shared
        .cohort_runtime(&agent_cohort)
        .and_then(|rt| {
            rt.host.lock().ok().map(|slot| match &*slot {
                HostSlot::Ready(h) => matches!(h.sandbox, HostSandbox::Wrapped(_)),
                _ => false,
            })
        })
        .unwrap_or(false);

    if is_sandboxed {
        let probe_read = host_tool(&engine, fixture.workspace(), "probe_read");
        let probe_write = host_tool(&engine, fixture.workspace(), "probe_write");
        let context =
            ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());

        let sibling_after = super::supervisor::host_cohort_data_dir(
            &fixture.home,
            HostCohort::agent("/proj/created-after-start"),
        )
        .unwrap();
        std::fs::create_dir_all(&sibling_after).unwrap();
        std::fs::write(sibling_after.join("sibling.txt"), "sibling secret").unwrap();

        let builtin_fixture_dir =
            super::supervisor::host_cohort_data_dir(&fixture.home, &HostCohort::Builtin).unwrap();
        std::fs::create_dir_all(&builtin_fixture_dir).unwrap();
        std::fs::write(builtin_fixture_dir.join("builtin.txt"), "builtin secret").unwrap();

        for (label, target) in [
            ("reviewed", dest_dir.join("existing.txt")),
            ("sibling", sibling_after.join("sibling.txt")),
            ("builtin", builtin_fixture_dir.join("builtin.txt")),
        ] {
            let res = probe_read
                .execute(json!({"path": target.to_string_lossy()}), &context)
                .await
                .unwrap();
            let payload: serde_json::Value = serde_json::from_str(&res.content).unwrap();
            assert_eq!(payload["ok"], false, "agent host cannot read {label} data");

            let write_target = target.with_file_name("infiltrate.txt");
            let res = probe_write
                .execute(json!({"path": write_target.to_string_lossy()}), &context)
                .await
                .unwrap();
            let payload: serde_json::Value = serde_json::from_str(&res.content).unwrap();
            assert_eq!(payload["ok"], false, "agent host cannot write {label} data");
        }

        let secret_probe_id = fixture.plugin_id("secret-probe");
        let own_data = super::supervisor::owner_data_dir(
            &fixture.home,
            &agent_cohort,
            &secret_probe_id,
            "secret-probe",
        );
        std::fs::create_dir_all(&own_data).unwrap();
        let res = probe_write
            .execute(
                json!({"path": own_data.join("probe.txt").to_string_lossy()}),
                &context,
            )
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        assert_eq!(payload["ok"], true, "agent host can write its own data");
    }

    let secret_probe_id = fixture.plugin_id("secret-probe");
    fixture.review_by_person("two-entries");
    fixture.review_by_person("secret-probe");
    engine.set_plugins(fixture.registry());
    engine.sync().await.ok();

    wait_host(&manager, || {
        let reg = manager.shared.registry.lock().unwrap();
        let failed = reg.owner(&plugin_id).is_some_and(
            |e| matches!(&e.state, OwnerState::Failed(msg) if msg.contains("already exists")),
        );
        let reviewed_ready = reg
            .owner(&secret_probe_id)
            .is_some_and(|e| e.state == OwnerState::Active && e.cohort == HostCohort::reviewed());
        failed && reviewed_ready
    })
    .await;

    assert_eq!(
        std::fs::read_to_string(dest_dir.join("existing.txt")).unwrap(),
        "pre-existing reviewed data"
    );
    assert!(!dest_dir.join("agent_file.txt").exists());
    assert_eq!(
        std::fs::read_to_string(agent_data.join("agent_file.txt")).unwrap(),
        "agent data"
    );

    let reviewed_sandboxed = manager
        .shared
        .cohort_runtime(&HostCohort::reviewed())
        .and_then(|rt| {
            rt.host.lock().ok().map(|slot| match &*slot {
                HostSlot::Ready(h) => matches!(h.sandbox, HostSandbox::Wrapped(_)),
                _ => false,
            })
        })
        .unwrap_or(false);
    if reviewed_sandboxed {
        let probe_read = host_tool(&engine, fixture.workspace(), "probe_read");
        let probe_write = host_tool(&engine, fixture.workspace(), "probe_write");
        let context =
            ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());

        let res = probe_read
            .execute(
                json!({"path": agent_data.join("agent_file.txt").to_string_lossy()}),
                &context,
            )
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        assert_eq!(payload["ok"], false, "reviewed host cannot read agent data");

        let res = probe_write
            .execute(
                json!({"path": agent_data.join("reviewed_write.txt").to_string_lossy()}),
                &context,
            )
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        assert_eq!(
            payload["ok"], false,
            "reviewed host cannot write agent data"
        );
    }
}

#[tokio::test]
async fn a_record_made_from_a_marked_shell_runs_in_the_agent_cohort_and_an_unmarked_one_in_the_reviewed_host()
 {
    let workspace = Path::new("/workspace/example");
    let approver_marked = PluginApprover::command_line(Some(workspace));
    assert_eq!(
        approver_marked.cohort(),
        PluginCohort::Agent {
            workspace: workspace.to_string_lossy().into_owned()
        }
    );
    assert_eq!(
        HostCohort::from_plugin_cohort(approver_marked.cohort()),
        HostCohort::agent(workspace)
    );

    let approver_unmarked = PluginApprover::command_line(None);
    assert_eq!(approver_unmarked.cohort(), PluginCohort::Reviewed);
    assert_eq!(
        HostCohort::from_plugin_cohort(approver_unmarked.cohort()),
        HostCohort::reviewed()
    );

    let approver_auto = PluginApprover::Agent(AgentApproval {
        posture: AgentPosture::AutoReview,
        workspace: workspace.to_string_lossy().into_owned(),
        thread_id: Some("th".into()),
        turn_id: Some("tu".into()),
        tool_call_id: Some("tc".into()),
    });
    assert_eq!(
        approver_auto.cohort(),
        PluginCohort::Agent {
            workspace: workspace.to_string_lossy().into_owned()
        }
    );

    let Some(node) = node_for_tests(
        "a_record_made_from_a_marked_shell_runs_in_the_agent_cohort_and_an_unmarked_one_in_the_reviewed_host",
    ) else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let fixture = CohortTestFixture::new(&[
        "two-entries",
        "child-persona",
        "plugin-context",
        "crash-tool",
    ])
    .await;
    fixture.review_by_marked_shell("two-entries");
    fixture.review_by_unmarked_shell("child-persona");
    fixture.review_by_auto_review("plugin-context", fixture.workspace());
    let other_workspace = fixture._temp.path().join("other-project");
    std::fs::create_dir_all(&other_workspace).unwrap();
    fixture.review_by_auto_review("crash-tool", &other_workspace);

    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();

    let pid_agent = cohort_pid(&manager, &HostCohort::agent(fixture.workspace()))
        .expect("marked runs in agent cohort");
    let pid_reviewed =
        cohort_pid(&manager, &HostCohort::reviewed()).expect("unmarked runs in reviewed cohort");
    assert_ne!(pid_agent, pid_reviewed);
    assert!(
        cohort_pid(&manager, &HostCohort::agent(&other_workspace)).is_none(),
        "other-project agent record must load nowhere"
    );
    let reg = manager.shared.registry.lock().unwrap();
    let context_id = fixture.plugin_id("plugin-context");
    assert_eq!(
        reg.owner(&context_id).map(|e| &e.cohort),
        Some(&HostCohort::agent(fixture.workspace())),
        "auto_review record runs in agent cohort"
    );
    let crash_id = fixture.plugin_id("crash-tool");
    assert!(
        reg.owner(&crash_id).is_none(),
        "different project record must not load"
    );
}
