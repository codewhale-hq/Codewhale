use super::*;
use crate::approval_log::ApprovalDecider;
use crate::conversation_modes::CreatorAvailabilityGuard;
use crate::core::authority::auto_review::AutoReviewPolicy;
use crate::core::authority::grants::GrantRef;
use crate::core::events::{ToolGate, ToolGateVerdict};
use crate::core::ops::{TurnInputAuthorship, TurnSpec, UserInputProvenance};
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use crate::hooks::HookExecutor;
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::tools::approval_cache::CREATOR_HUMAN_APPROVAL_KEY_PREFIX;
use crate::tools::spec::{ToolError, ToolResult};
use crate::tools::subagent::SubAgentRuntime;
use crate::tools::subagent::engine::ChildAuthority;
use codewhale_agent_contract::role::FleetRole;
use codewhale_config::ProviderKind;
use codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL;
use codewhale_config::extension_host::ExtensionHostRuntime;
use codewhale_config::hooks::{Hook, HookEvent, HooksConfig};
use codewhale_execpolicy::{ExecPolicyEngine, PermissionAction, Ruleset, ToolAskRule};
use codewhale_net::network_policy::{NetworkPolicy, NetworkPolicyDecider};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::host_services::{
    AgentApproval, AgentPosture, PersonVia, PluginApprover, ReviewApproval,
};
use codewhale_runtime::config::{AutoReviewConfig, AutoReviewRuleConfig};
use codewhale_skill_install::bounded_tar::test_archives::archive_from_entries;
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct GateFixture {
    _home: crate::test_support::SealedHome,
    _policy: TestPolicyGuard,
    _available: CreatorAvailabilityGuard,
    temp: tempfile::TempDir,
    workspace: PathBuf,
    user_plugins_dir: PathBuf,
    builtin_dir: PathBuf,
    state_path: PathBuf,
    reg_config: DiscoveryConfig,
    manager: Arc<ExtensionHostManager>,
    _manager_guard: TestManagerGuard,
}

impl GateFixture {
    fn new(test_name: &str) -> Option<Self> {
        let home = crate::test_support::SealedHome::new();
        let node = crate::extension_host::test_support::node_for_tests(test_name)?;
        let policy = TestPolicyGuard::extension_host(false);
        let available = CreatorAvailabilityGuard::set(None);
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("ws");
        let user_plugins_dir = temp.path().join("user-plugins");
        let workspace_plugins_dir = workspace.join(".codewhale/plugins");
        let builtin_dir = temp.path().join("builtin-plugins");
        let state_path = temp.path().join("state/plugin-state.json");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&user_plugins_dir).expect("user plugins");
        fs::create_dir_all(&workspace_plugins_dir).expect("workspace plugins");
        fs::create_dir_all(&builtin_dir).expect("builtin plugins");

        let reg_config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: user_plugins_dir.clone(),
            workspace_plugins_dir,
            builtin_plugin_dirs: vec![builtin_dir.clone()],
            state_path: state_path.clone(),
        };

        let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(temp.path().join("ext-root")),
            ..Default::default()
        }));
        let manager_guard = TestManagerGuard::install(Arc::clone(&manager));

        Some(Self {
            _home: home,
            _policy: policy,
            _available: available,
            temp,
            workspace,
            user_plugins_dir,
            builtin_dir,
            state_path,
            reg_config,
            manager,
            _manager_guard: manager_guard,
        })
    }

    fn discover(&self) -> PluginRegistry {
        discover_with_config(&self.reg_config)
    }

    fn engine_config(&self, registry: PluginRegistry) -> EngineConfig {
        let mut cfg = deterministic_engine_config(&self.workspace);
        cfg.features.enable(Feature::PluginCreator);
        cfg.features.disable(Feature::ExtensionHost);
        cfg.plugin_registry = Some(Arc::new(registry));
        cfg.agent_preset = Some("creator".to_string());
        cfg
    }
}

fn write_skill_plugin(dir: &Path, name: &str) {
    let skills_dir = dir.join(format!("skills/{name}-skill"));
    fs::create_dir_all(&skills_dir).expect("skills dir");
    fs::write(
        dir.join("plugin.toml"),
        format!(
            "schema_version = 1\n[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\n\n[skills]\npath = \"skills\"\n"
        ),
    )
    .expect("plugin.toml");
    fs::write(
        skills_dir.join("SKILL.md"),
        format!("---\nname: {name}-skill\ndescription: demo skill for {name}\n---\nBody\n"),
    )
    .expect("SKILL.md");
}

fn write_dsh_npm_plugin(dir: &Path, name: &str) {
    let lib_dir = dir.join("lib");
    fs::create_dir_all(&lib_dir).expect("lib dir");
    let pkg = serde_json::json!({
        "name": format!("@demo/{name}"),
        "version": "1.0.0",
        "main": "lib/index.js",
        "dependencies": { "zod": "^4.0.0" },
        "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
    });
    fs::write(dir.join("package.json"), pkg.to_string()).expect("package.json");
    fs::write(
        dir.join("cordis.patch.yml"),
        "- insert:\n  - id: entry\n    name: ./lib/index.js\n",
    )
    .expect("cordis.patch.yml");
    fs::write(
        lib_dir.join("index.js"),
        "import 'zod';\nexport function apply() {}\n",
    )
    .expect("index.js");
}

fn read_state_entry(state_path: &Path, plugin_name: &str) -> Option<Value> {
    let raw = fs::read_to_string(state_path).ok()?;
    let state: Value = serde_json::from_str(&raw).ok()?;
    let plugins = state.get("plugins")?.as_object()?;
    let suffix = format!("/{plugin_name}");
    plugins
        .iter()
        .find(|(key, _)| key.as_str() == plugin_name || key.ends_with(&suffix))
        .map(|(_, value)| value.clone())
}

fn parse_approver(value: Option<&Value>) -> Option<PluginApprover> {
    value.and_then(|item| serde_json::from_value(item.clone()).ok())
}

#[derive(Clone, Copy)]
enum ApprovalBehavior {
    PanicOnCard,
    ApproveUser,
    DenyUser,
    ApproveBy(ApprovalDecider),
}

#[derive(Clone)]
struct CapturedCard {
    tool_name: String,
    description: String,
    approval_key: String,
    approval_force_prompt: bool,
}

struct GateReceipt {
    gate: ToolGate,
    decision: ToolGateVerdict,
    reason: String,
}

struct TurnOutcome {
    cards: Vec<CapturedCard>,
    tool_results: Vec<(String, Result<ToolResult, ToolError>)>,
    gate_receipts: Vec<GateReceipt>,
}

fn build_turn_op(
    content: &str,
    mode: AppMode,
    approval_mode: ApprovalMode,
    config: &Config,
    provenance: UserInputProvenance,
    hook_executor: Option<Arc<HookExecutor>>,
) -> Op {
    let mut op = external_user_message_op(content, mode, config);
    if let Op::SendMessage(TurnSpec {
        approval_mode: turn_approval,
        auto_approve,
        trust_mode,
        provenance: turn_provenance,
        hook_executor: turn_hooks,
        input_authorship,
        ..
    }) = &mut op
    {
        *turn_approval = approval_mode;
        *auto_approve = approval_mode == ApprovalMode::Bypass;
        *trust_mode = approval_mode == ApprovalMode::Bypass;
        *turn_provenance = provenance;
        *turn_hooks = hook_executor;
        *input_authorship = if provenance == UserInputProvenance::ExternalUser {
            TurnInputAuthorship::Person(content.to_string())
        } else {
            TurnInputAuthorship::NotPerson
        };
    }
    op
}

async fn drive_turn_with_hook<F>(
    handle: &EngineHandle,
    op: Op,
    behavior: ApprovalBehavior,
    mut on_card: F,
) -> TurnOutcome
where
    F: FnMut(&EngineHandle, &CapturedCard),
{
    handle.send(op).await.expect("send turn op");
    let mut cards = Vec::new();
    let mut tool_results = Vec::new();
    let mut gate_receipts = Vec::new();
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired {
                id,
                tool_name,
                description,
                approval_key,
                approval_force_prompt,
                ..
            } => {
                let card = CapturedCard {
                    tool_name,
                    description,
                    approval_key,
                    approval_force_prompt,
                };
                on_card(handle, &card);
                cards.push(card);
                match behavior {
                    ApprovalBehavior::PanicOnCard => {
                        panic!(
                            "unexpected approval card: {:?}",
                            cards.last().map(|c| &c.tool_name)
                        );
                    }
                    ApprovalBehavior::ApproveUser => {
                        handle.approve_tool_call(id).await.expect("approve user");
                    }
                    ApprovalBehavior::DenyUser => {
                        handle.deny_tool_call(id).await.expect("deny user");
                    }
                    ApprovalBehavior::ApproveBy(decider) => {
                        handle
                            .approve_tool_call_by_grant(
                                id,
                                decider,
                                GrantRef {
                                    grant_id: "grant-test".to_string(),
                                    label: "remembered test grant".to_string(),
                                },
                            )
                            .await
                            .expect("approve by non-user decider");
                    }
                }
            }
            Event::ToolCallComplete { name, result, .. } => {
                tool_results.push((name, result));
            }
            Event::ToolGateDecision {
                gate,
                decision,
                reason,
                ..
            } => {
                gate_receipts.push(GateReceipt {
                    gate,
                    decision,
                    reason,
                });
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    TurnOutcome {
        cards,
        tool_results,
        gate_receipts,
    }
}

async fn drive_turn(handle: &EngineHandle, op: Op, behavior: ApprovalBehavior) -> TurnOutcome {
    drive_turn_with_hook(handle, op, behavior, |_, _| {}).await
}

fn spawn_interactive(
    cfg: EngineConfig,
    mock: Arc<MockLlmClient>,
) -> (EngineHandle, tokio::task::JoinHandle<()>) {
    let config = Config::default();
    let (mut engine, mut handle) = Engine::new_with_model_client(cfg, &config, mock);
    engine.attach_owner_surface_for_tests(&mut handle);
    let task = tokio::spawn(engine.run());
    (handle, task)
}

async fn shutdown_engine(handle: EngineHandle, task: tokio::task::JoinHandle<()>) {
    handle.send(Op::Shutdown).await.expect("shutdown");
    tokio::time::timeout(model_turn_event_timeout(), task)
        .await
        .expect("shutdown timeout")
        .expect("engine task");
}

fn assert_permission_denied(outcome: &TurnOutcome) {
    assert_eq!(outcome.tool_results.len(), 1, "expected one tool result");
    let (_, result) = &outcome.tool_results[0];
    let err = result.as_ref().expect_err("expected permission denied");
    assert!(
        matches!(err, ToolError::PermissionDenied { .. }),
        "expected PermissionDenied, got: {err:?}"
    );
}

fn assert_creator_card(card: &CapturedCard) {
    assert_eq!(card.tool_name, "plugin_manager");
    assert!(
        card.approval_key
            .starts_with(CREATOR_HUMAN_APPROVAL_KEY_PREFIX),
        "unexpected approval key: {}",
        card.approval_key
    );
    assert!(card.approval_force_prompt, "creator card must force prompt");
}

fn start_tarball_server(
    archive: Vec<u8>,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let hits = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let hits_srv = Arc::clone(&hits);
    let stop_srv = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    hits_srv.fetch_add(1, Ordering::SeqCst);
                    let mut request = Vec::new();
                    let mut buf = [0_u8; 1024];
                    loop {
                        let read = stream.read(&mut buf).unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buf[..read]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        archive.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&archive);
                    let _ = stream.flush();
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (
        format!("http://127.0.0.1:{port}/remote-skill.tar.gz"),
        hits,
        stop,
        server,
    )
}

#[tokio::test]
async fn read_only_plugin_queries_execute_with_zero_cards_across_modes() {
    let Some(f) = GateFixture::new("read_only_plugin_queries_execute_with_zero_cards_across_modes")
    else {
        return;
    };
    let installed_dir = f.user_plugins_dir.join("alpha-listed");
    write_skill_plugin(&installed_dir, "alpha-listed");

    let config = Config::default();
    for mode in [
        ApprovalMode::Suggest,
        ApprovalMode::AcceptEdits,
        ApprovalMode::Auto,
        ApprovalMode::Bypass,
        ApprovalMode::Never,
    ] {
        for action in ["list_plugins", "list_bundles"] {
            let mock = Arc::new(MockLlmClient::new(vec![
                canned::tool_call_turn(
                    "call-list",
                    "plugin_manager",
                    &serde_json::json!({ "action": action }).to_string(),
                ),
                canned::simple_text_turn("listed"),
            ]));
            let (handle, task) = spawn_interactive(f.engine_config(f.discover()), mock.clone());
            let op = build_turn_op(
                "list plugins",
                AppMode::Agent,
                mode,
                &config,
                UserInputProvenance::ExternalUser,
                None,
            );
            let outcome = drive_turn(&handle, op, ApprovalBehavior::PanicOnCard).await;
            shutdown_engine(handle, task).await;

            assert!(outcome.cards.is_empty(), "{mode:?}/{action}");
            assert_eq!(outcome.tool_results.len(), 1);
            let res = outcome.tool_results[0]
                .1
                .as_ref()
                .expect("read query succeeds");
            assert!(res.success, "{mode:?}/{action}: {}", res.content);
            assert!(
                res.content.contains("alpha-listed"),
                "{mode:?}/{action}: {}",
                res.content
            );
            assert_eq!(
                mock.captured_requests().len(),
                2,
                "{mode:?}/{action} must not consult guardian"
            );
        }
    }

    f.manager.shutdown().await;
}

#[tokio::test]
async fn ask_and_accept_edits_prompt_human_card_approve_and_deny() {
    let Some(f) = GateFixture::new("ask_and_accept_edits_prompt_human_card_approve_and_deny")
    else {
        return;
    };
    let src_dir = f.workspace.join("plugins/alpha-skill");
    write_skill_plugin(&src_dir, "alpha-skill");
    let install_args = serde_json::json!({
        "action": "install_bundle",
        "target": src_dir.to_string_lossy(),
    })
    .to_string();

    let config = Config::default();

    let deny_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-deny", "plugin_manager", &install_args),
        canned::simple_text_turn("denied"),
    ]));
    let (deny_handle, deny_task) = spawn_interactive(f.engine_config(f.discover()), deny_mock);
    let deny_outcome = drive_turn(
        &deny_handle,
        build_turn_op(
            "install alpha-skill",
            AppMode::Agent,
            ApprovalMode::Suggest,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::DenyUser,
    )
    .await;
    shutdown_engine(deny_handle, deny_task).await;
    assert_eq!(deny_outcome.cards.len(), 1);
    assert_creator_card(&deny_outcome.cards[0]);
    assert!(
        deny_outcome.cards[0]
            .description
            .contains("Action: install")
    );
    assert!(deny_outcome.cards[0].description.contains("alpha-skill"));
    assert_permission_denied(&deny_outcome);
    assert!(f.discover().get("alpha-skill").is_none());
    assert!(!f.state_path.exists());

    let allow_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-allow", "plugin_manager", &install_args),
        canned::simple_text_turn("installed"),
    ]));
    let (allow_handle, allow_task) = spawn_interactive(f.engine_config(f.discover()), allow_mock);
    let allow_outcome = drive_turn(
        &allow_handle,
        build_turn_op(
            "install alpha-skill",
            AppMode::Agent,
            ApprovalMode::Suggest,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(allow_handle, allow_task).await;
    assert_eq!(allow_outcome.cards.len(), 1);
    assert_creator_card(&allow_outcome.cards[0]);
    let res = allow_outcome.tool_results[0]
        .1
        .as_ref()
        .expect("install ok");
    assert!(res.success, "{}", res.content);

    let entry = read_state_entry(&f.state_path, "alpha-skill").expect("state entry");
    assert_eq!(entry["enabled"], serde_json::json!(true));
    assert_eq!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    let disable_args = serde_json::json!({
        "action": "set_bundle",
        "target": "alpha-skill",
        "enabled": false,
    })
    .to_string();
    let ae_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-ae-toggle", "plugin_manager", &disable_args),
        canned::simple_text_turn("disabled"),
    ]));
    let (ae_handle, ae_task) = spawn_interactive(f.engine_config(f.discover()), ae_mock);
    let ae_outcome = drive_turn(
        &ae_handle,
        build_turn_op(
            "disable alpha-skill",
            AppMode::Agent,
            ApprovalMode::AcceptEdits,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(ae_handle, ae_task).await;
    assert_eq!(ae_outcome.cards.len(), 1, "Accept Edits must prompt");
    assert_creator_card(&ae_outcome.cards[0]);
    assert!(
        !f.discover()
            .get("alpha-skill")
            .expect("still installed")
            .enabled
    );

    f.manager.shutdown().await;
}

#[tokio::test]
async fn auto_review_inside_scope_consults_guardian_and_allows_or_denies() {
    let Some(f) =
        GateFixture::new("auto_review_inside_scope_consults_guardian_and_allows_or_denies")
    else {
        return;
    };
    let src_dir = f.workspace.join("plugins/scoped-skill");
    write_skill_plugin(&src_dir, "scoped-skill");
    let install_args = serde_json::json!({
        "action": "install_bundle",
        "target": src_dir.to_string_lossy(),
    })
    .to_string();
    let config = Config::default();

    let deny_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-ar-deny", "plugin_manager", &install_args),
        canned::simple_text_turn("blocked by guardian"),
    ]));
    deny_mock.push_message_response(guardian_fixture_response(
        r#"{"risk_level":"high","decision":"deny","reason":"unexpected capability mutation"}"#,
    ));
    let (deny_handle, deny_task) =
        spawn_interactive(f.engine_config(f.discover()), deny_mock.clone());
    let deny_outcome = drive_turn(
        &deny_handle,
        auto_review_message_op("Install scoped-skill cleanly", &config),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(deny_handle, deny_task).await;
    assert!(deny_outcome.cards.is_empty());
    assert_permission_denied(&deny_outcome);
    assert!(f.discover().get("scoped-skill").is_none());
    let deny_reqs = deny_mock.captured_requests();
    assert_eq!(deny_reqs.len(), 3, "main, guardian, follow-up");
    assert!(deny_reqs[1].tools.is_none());

    let allow_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-ar-allow", "plugin_manager", &install_args),
        canned::simple_text_turn("guardian installed"),
    ]));
    allow_mock.push_message_response(guardian_fixture_response(
        r#"{"risk_level":"low","decision":"allow","reason":"clean workspace skill bundle"}"#,
    ));
    let (allow_handle, allow_task) =
        spawn_interactive(f.engine_config(f.discover()), allow_mock.clone());
    let allow_outcome = drive_turn(
        &allow_handle,
        auto_review_message_op("Install scoped-skill", &config),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(allow_handle, allow_task).await;
    assert!(allow_outcome.cards.is_empty());
    assert!(
        allow_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("install ok")
            .success
    );
    assert!(allow_outcome.gate_receipts.iter().any(|r| {
        r.gate == ToolGate::AutoReviewGuardian
            && r.decision == ToolGateVerdict::Allowed
            && !r.reason.is_empty()
    }));
    let allow_reqs = allow_mock.captured_requests();
    assert_eq!(allow_reqs.len(), 3);
    let guardian_json = serde_json::to_string(&allow_reqs[1]).expect("guardian json");
    assert!(guardian_json.contains("plugin_review"), "{guardian_json}");
    assert!(
        guardian_json.contains("untrusted_evidence"),
        "{guardian_json}"
    );

    let entry = read_state_entry(&f.state_path, "scoped-skill").expect("state entry");
    assert_eq!(entry["enabled"], serde_json::json!(true));
    assert!(matches!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::AutoReview,
            ..
        }))
    ));

    f.manager.shutdown().await;
}

#[tokio::test]
async fn auto_review_allow_rule_allows_inside_scope_but_never_bypasses_outside_scope() {
    let Some(f) = GateFixture::new(
        "auto_review_allow_rule_allows_inside_scope_but_never_bypasses_outside_scope",
    ) else {
        return;
    };
    let inside_dir = f.workspace.join("plugins/rule-inside");
    write_skill_plugin(&inside_dir, "rule-inside");
    let outside_dir = f.temp.path().join("outside/rule-outside");
    write_skill_plugin(&outside_dir, "rule-outside");

    let mut rule_cfg = Config::default();
    let mut rule = AutoReviewRuleConfig::default();
    rule.id = Some("allow-pm".to_string());
    rule.tool = Some("plugin_manager".to_string());
    rule.reason = Some("allow creator".to_string());
    rule_cfg.auto_review = Some(AutoReviewConfig {
        allow: vec![rule],
        ..Default::default()
    });
    let allow_policy = AutoReviewPolicy::from_config(&rule_cfg);
    let config = Config::default();

    let mut inside_cfg = f.engine_config(f.discover());
    inside_cfg.auto_review_policy = allow_policy.clone();
    let inside_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-rule-in",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": inside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("allowed by rule"),
    ]));
    let (in_handle, in_task) = spawn_interactive(inside_cfg, inside_mock.clone());
    let in_outcome = drive_turn(
        &in_handle,
        auto_review_message_op("install inside", &config),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(in_handle, in_task).await;
    assert!(in_outcome.cards.is_empty());
    assert!(
        in_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("rule allowed inside")
            .success
    );
    assert_eq!(
        inside_mock.captured_requests().len(),
        2,
        "allow rule must allow inside-scope without guardian"
    );
    let inside_entry = read_state_entry(&f.state_path, "rule-inside").expect("inside entry");
    assert!(matches!(
        parse_approver(inside_entry["trust"].get("approved_by")),
        Some(PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::AutoReview,
            ..
        }))
    ));

    let mut outside_cfg = f.engine_config(f.discover());
    outside_cfg.auto_review_policy = allow_policy;
    let outside_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-rule-out",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": outside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("card approved"),
    ]));
    let (out_handle, out_task) = spawn_interactive(outside_cfg, outside_mock.clone());
    let out_outcome = drive_turn(
        &out_handle,
        auto_review_message_op("install outside", &config),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(out_handle, out_task).await;
    assert_eq!(out_outcome.cards.len(), 1);
    assert_creator_card(&out_outcome.cards[0]);
    assert!(
        out_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("person approved")
            .success
    );
    assert_eq!(
        outside_mock.captured_requests().len(),
        2,
        "outside scope must skip guardian"
    );
    let outside_entry = read_state_entry(&f.state_path, "rule-outside").expect("outside entry");
    assert_eq!(
        parse_approver(outside_entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    f.manager.shutdown().await;
}

#[tokio::test]
async fn auto_review_outside_scope_skips_guardian_prompts_interactively_and_denies_unattended() {
    let Some(f) = GateFixture::new(
        "auto_review_outside_scope_skips_guardian_prompts_interactively_and_denies_unattended",
    ) else {
        return;
    };
    let outside_dir = f.temp.path().join("outside/untrusted-ext");
    write_skill_plugin(&outside_dir, "untrusted-ext");
    let args = serde_json::json!({
        "action": "install_bundle",
        "target": outside_dir.to_string_lossy(),
    })
    .to_string();
    let config = Config::default();

    let headless_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-headless", "plugin_manager", &args),
        canned::simple_text_turn("refused headless"),
    ]));
    let (mut headless_engine, mut headless_handle) = Engine::new_with_model_client(
        f.engine_config(f.discover()),
        &config,
        headless_mock.clone(),
    );
    headless_engine.host_profile = EngineHostProfile::Headless;
    headless_handle.set_host_profile(EngineHostProfile::Headless);
    let headless_task = tokio::spawn(headless_engine.run());
    let headless_outcome = drive_turn(
        &headless_handle,
        auto_review_message_op("install outside headless", &config),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(headless_handle, headless_task).await;
    assert!(headless_outcome.cards.is_empty());
    assert_permission_denied(&headless_outcome);
    assert_eq!(headless_mock.captured_requests().len(), 2);
    assert!(f.discover().get("untrusted-ext").is_none());

    let interactive_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-int", "plugin_manager", &args),
        canned::simple_text_turn("approved interactively"),
    ]));
    let (int_handle, int_task) =
        spawn_interactive(f.engine_config(f.discover()), interactive_mock.clone());
    let int_outcome = drive_turn(
        &int_handle,
        auto_review_message_op("install outside interactively", &config),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(int_handle, int_task).await;
    assert_eq!(int_outcome.cards.len(), 1);
    assert_creator_card(&int_outcome.cards[0]);
    assert!(int_outcome.tool_results[0].1.as_ref().expect("ok").success);
    assert_eq!(interactive_mock.captured_requests().len(), 2);
    let entry = read_state_entry(&f.state_path, "untrusted-ext").expect("state entry");
    assert_eq!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    f.manager.shutdown().await;
}

#[tokio::test]
async fn full_access_inside_scope_runs_without_card_and_records_agent_approval() {
    let Some(f) =
        GateFixture::new("full_access_inside_scope_runs_without_card_and_records_agent_approval")
    else {
        return;
    };
    let inside_dir = f.workspace.join("plugins/fa-local");
    write_skill_plugin(&inside_dir, "fa-local");
    let config = Config::default();

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-fa-install",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": inside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("installed fa"),
        canned::tool_call_turn(
            "call-fa-disable",
            "plugin_manager",
            &serde_json::json!({
                "action": "set_bundle",
                "target": "fa-local",
                "enabled": false,
            })
            .to_string(),
        ),
        canned::simple_text_turn("disabled fa"),
    ]));
    let (handle, task) = spawn_interactive(f.engine_config(f.discover()), mock);

    let install_outcome = drive_turn(
        &handle,
        build_turn_op(
            "install fa-local",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    assert!(install_outcome.cards.is_empty());
    assert!(
        install_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("ok")
            .success
    );
    let entry = read_state_entry(&f.state_path, "fa-local").expect("fa entry");
    assert_eq!(entry["enabled"], serde_json::json!(true));
    assert!(matches!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::FullAccess,
            ..
        }))
    ));

    let toggle_outcome = drive_turn(
        &handle,
        build_turn_op(
            "disable fa-local",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;
    assert!(toggle_outcome.cards.is_empty());
    assert!(
        toggle_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("ok")
            .success
    );
    assert!(!f.discover().get("fa-local").expect("present").enabled);

    f.manager.shutdown().await;
}

#[tokio::test]
async fn full_access_outside_scope_asks_the_person() {
    let Some(f) = GateFixture::new("full_access_outside_scope_asks_the_person") else {
        return;
    };
    let config = Config::default();

    let outside_dir = f.temp.path().join("external/outside-fa");
    write_skill_plugin(&outside_dir, "outside-fa");
    let out_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-out-dir",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": outside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("installed outside"),
    ]));
    let (out_handle, out_task) = spawn_interactive(f.engine_config(f.discover()), out_mock);
    let out_outcome = drive_turn(
        &out_handle,
        build_turn_op(
            "install outside dir",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(out_handle, out_task).await;
    assert_eq!(out_outcome.cards.len(), 1);
    assert_creator_card(&out_outcome.cards[0]);
    assert!(out_outcome.tool_results[0].1.as_ref().expect("ok").success);
    let out_entry = read_state_entry(&f.state_path, "outside-fa").expect("outside-fa entry");
    assert_eq!(
        parse_approver(out_entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    let person_dir = f.user_plugins_dir.join("person-approved");
    write_skill_plugin(&person_dir, "person-approved");
    let builtin_pack = f.builtin_dir.join("core-pack");
    write_skill_plugin(&builtin_pack, "core-pack");
    {
        let mut reg = f.discover();
        reg.trust_and_enable_by(
            "person-approved",
            ReviewApproval {
                approved_by: PluginApprover::person(PersonVia::Terminal),
                host_services: None,
            },
            PluginApprover::person(PersonVia::Terminal),
        )
        .expect("trust person plugin");
        reg.disable("person-approved").expect("disable before test");
    }

    let toggle_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-person-toggle",
            "plugin_manager",
            &serde_json::json!({
                "action": "set_bundle",
                "target": "person-approved",
                "enabled": true,
            })
            .to_string(),
        ),
        canned::simple_text_turn("toggled person"),
    ]));
    let (person_handle, person_task) =
        spawn_interactive(f.engine_config(f.discover()), toggle_mock);
    let person_outcome = drive_turn(
        &person_handle,
        build_turn_op(
            "enable person-approved",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(person_handle, person_task).await;
    assert_eq!(person_outcome.cards.len(), 1);
    assert_creator_card(&person_outcome.cards[0]);
    assert!(
        person_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("ok")
            .success
    );
    let person_entry = read_state_entry(&f.state_path, "person-approved").expect("person entry");
    assert_eq!(
        parse_approver(person_entry.get("enabled_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    let builtin_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-builtin-toggle",
            "plugin_manager",
            &serde_json::json!({
                "action": "set_bundle",
                "target": "core-pack",
                "enabled": true,
            })
            .to_string(),
        ),
        canned::simple_text_turn("enabled builtin"),
    ]));
    let (builtin_handle, builtin_task) =
        spawn_interactive(f.engine_config(f.discover()), builtin_mock);
    let builtin_outcome = drive_turn(
        &builtin_handle,
        build_turn_op(
            "enable core-pack",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(builtin_handle, builtin_task).await;
    assert_eq!(builtin_outcome.cards.len(), 1);
    assert_creator_card(&builtin_outcome.cards[0]);
    assert!(
        builtin_outcome.tool_results[0]
            .1
            .as_ref()
            .expect("ok")
            .success
    );
    let builtin_entry = read_state_entry(&f.state_path, "core-pack").expect("builtin entry");
    assert_eq!(
        parse_approver(builtin_entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    let npm_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-npm",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": "npm:@demo/outside-remote@1.0.0",
            })
            .to_string(),
        ),
        canned::simple_text_turn("denied npm"),
    ]));
    let (npm_handle, npm_task) = spawn_interactive(f.engine_config(f.discover()), npm_mock);
    let npm_outcome = drive_turn(
        &npm_handle,
        build_turn_op(
            "install npm pkg",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::DenyUser,
    )
    .await;
    shutdown_engine(npm_handle, npm_task).await;
    assert_eq!(npm_outcome.cards.len(), 1);
    assert_creator_card(&npm_outcome.cards[0]);
    assert!(
        npm_outcome.cards[0]
            .description
            .contains("registry.npmjs.org")
    );
    assert!(
        npm_outcome.cards[0]
            .description
            .contains("disabled and untrusted")
    );
    assert_permission_denied(&npm_outcome);

    f.manager.shutdown().await;
}

#[tokio::test]
async fn a_remote_source_is_not_fetched_before_a_decision_and_no_yes_covers_unseen_bytes() {
    let Some(f) = GateFixture::new(
        "a_remote_source_is_not_fetched_before_a_decision_and_no_yes_covers_unseen_bytes",
    ) else {
        return;
    };
    let archive = archive_from_entries(&[
        (
            "remote-skill/plugin.toml",
            b"schema_version = 1\n[plugin]\nname = \"remote-skill\"\nversion = \"1.0.0\"\n",
        ),
        (
            "remote-skill/skills/helper/SKILL.md",
            b"---\nname: helper\ndescription: remote skill\n---\nbody\n",
        ),
    ]);
    let (url, hits, stop, server) = start_tarball_server(archive);
    let config = Config::default();

    let deny_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-remote-deny",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": url,
            })
            .to_string(),
        ),
        canned::simple_text_turn("remote denied"),
    ]));
    let (deny_handle, deny_task) = spawn_interactive(f.engine_config(f.discover()), deny_mock);
    let deny_outcome = drive_turn(
        &deny_handle,
        build_turn_op(
            "install remote deny",
            AppMode::Agent,
            ApprovalMode::Suggest,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::DenyUser,
    )
    .await;
    shutdown_engine(deny_handle, deny_task).await;
    assert_eq!(deny_outcome.cards.len(), 1);
    assert_creator_card(&deny_outcome.cards[0]);
    assert_permission_denied(&deny_outcome);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "remote archive must never be fetched before approval"
    );
    assert!(f.discover().get("remote-skill").is_none());

    let allow_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-remote-allow",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": url,
            })
            .to_string(),
        ),
        canned::simple_text_turn("remote landed disabled"),
    ]));
    let mut allow_cfg = f.engine_config(f.discover());
    allow_cfg.network_policy = Some(NetworkPolicyDecider::new(
        NetworkPolicy {
            allow: vec!["127.0.0.1".to_string()],
            ..NetworkPolicy::default()
        },
        None,
    ));
    let (allow_handle, allow_task) = spawn_interactive(allow_cfg, allow_mock);
    let mut hits_when_card_opened = None;
    let allow_outcome = drive_turn_with_hook(
        &allow_handle,
        build_turn_op(
            "install remote allow",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
        |_, _| {
            hits_when_card_opened = Some(hits.load(Ordering::SeqCst));
        },
    )
    .await;
    shutdown_engine(allow_handle, allow_task).await;
    stop.store(true, Ordering::SeqCst);
    server.join().expect("server thread");

    assert_eq!(allow_outcome.cards.len(), 1);
    assert_creator_card(&allow_outcome.cards[0]);
    assert_eq!(hits_when_card_opened, Some(0));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let res = allow_outcome.tool_results[0]
        .1
        .as_ref()
        .expect("install ok");
    assert!(res.success, "{}", res.content);
    assert!(res.content.contains("/plugin trust"), "{}", res.content);

    let fresh = f.discover();
    let loaded = fresh.get("remote-skill").expect("remote plugin installed");
    assert!(!loaded.enabled, "unseen remote bytes must land disabled");
    assert_eq!(
        loaded.approved_by, None,
        "unseen remote bytes stay untrusted"
    );
    if let Some(entry) = read_state_entry(&f.state_path, "remote-skill") {
        assert_ne!(entry["enabled"], serde_json::json!(true));
        assert!(entry["trust"].get("approved_by").is_none_or(Value::is_null));
    }

    let dsh_dir = f.workspace.join("plugins/dsh-npm");
    write_dsh_npm_plugin(&dsh_dir, "dsh-npm");
    let dsh_args = serde_json::json!({
        "action": "install_bundle",
        "target": dsh_dir.to_string_lossy(),
    })
    .to_string();
    let dsh_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-dsh-npm", "plugin_manager", &dsh_args),
        canned::simple_text_turn("dsh npm denied"),
    ]));
    let (dsh_handle, dsh_task) = spawn_interactive(f.engine_config(f.discover()), dsh_mock.clone());
    let dsh_outcome = drive_turn(
        &dsh_handle,
        auto_review_message_op("install local dsh with npm deps", &config),
        ApprovalBehavior::DenyUser,
    )
    .await;
    shutdown_engine(dsh_handle, dsh_task).await;
    assert_eq!(dsh_outcome.cards.len(), 1);
    assert_creator_card(&dsh_outcome.cards[0]);
    assert!(
        dsh_outcome.cards[0]
            .description
            .contains("registry.npmjs.org")
    );
    assert!(
        dsh_outcome.cards[0]
            .description
            .contains("disabled and untrusted")
    );
    assert_eq!(dsh_mock.captured_requests().len(), 2);
    assert_permission_denied(&dsh_outcome);

    f.manager.shutdown().await;
}

#[tokio::test]
async fn plan_never_background_subagent_and_nested_calls_refuse_plugin_changes() {
    let Some(f) =
        GateFixture::new("plan_never_background_subagent_and_nested_calls_refuse_plugin_changes")
    else {
        return;
    };
    let src_dir = f.workspace.join("plugins/blocked-skill");
    write_skill_plugin(&src_dir, "blocked-skill");
    let outside_dir = f.temp.path().join("outside/blocked-outside");
    write_skill_plugin(&outside_dir, "blocked-outside");
    let inside_args = serde_json::json!({
        "action": "install_bundle",
        "target": src_dir.to_string_lossy(),
    })
    .to_string();
    let outside_args = serde_json::json!({
        "action": "install_bundle",
        "target": outside_dir.to_string_lossy(),
    })
    .to_string();
    let config = Config::default();

    for (app_mode, approval_mode, provenance, args) in [
        (
            AppMode::Plan,
            ApprovalMode::Suggest,
            UserInputProvenance::ExternalUser,
            &inside_args,
        ),
        (
            AppMode::Agent,
            ApprovalMode::Never,
            UserInputProvenance::ExternalUser,
            &inside_args,
        ),
        (
            AppMode::Agent,
            ApprovalMode::Suggest,
            UserInputProvenance::Runtime,
            &inside_args,
        ),
        (
            AppMode::Agent,
            ApprovalMode::Bypass,
            UserInputProvenance::Runtime,
            &outside_args,
        ),
    ] {
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call-refuse", "plugin_manager", args),
            canned::simple_text_turn("refused"),
        ]));
        let (handle, task) = spawn_interactive(f.engine_config(f.discover()), mock);
        let outcome = drive_turn(
            &handle,
            build_turn_op(
                "blocked",
                app_mode,
                approval_mode,
                &config,
                provenance,
                None,
            ),
            ApprovalBehavior::PanicOnCard,
        )
        .await;
        shutdown_engine(handle, task).await;
        assert!(outcome.cards.is_empty(), "{app_mode:?}/{approval_mode:?}");
        assert_permission_denied(&outcome);
    }

    let parent_mock = Arc::new(MockLlmClient::new(vec![]));
    let mut parent_cfg = f.engine_config(f.discover());
    parent_cfg.session_id = Some("origin-creator-gate".into());
    parent_cfg.allow_shell = true;
    let (parent_engine, _parent_handle) =
        Engine::new_with_model_client(parent_cfg, &config, parent_mock);
    let api = Config::default().with_legacy_root(Some("creator-gate-child".into()), None);
    let client = parent_engine
        .codewhale_client
        .clone()
        .unwrap_or_else(|| crate::client::CodewhaleClient::new(&api).expect("stub client"));
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: DEFAULT_TEXT_MODEL.into(),
        capabilities: Default::default(),
        limits: None,
        client: Some(client.clone()),
        api_config: Box::new(api.clone()),
        locale_tag: parent_engine.config.locale_tag.clone(),
        role_models: HashMap::new(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let parent_policy = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let parent_ctx = parent_engine.build_tool_context_for_turn(&parent_policy, &route);
    let runtime = SubAgentRuntime::new(
        client,
        DEFAULT_TEXT_MODEL.into(),
        parent_ctx,
        true,
        None,
        parent_engine.subagent_manager.clone(),
    )
    .with_api_config(api.clone())
    .with_approval_receipt_store(parent_engine.approval_receipt_store.clone());
    let mut captured = ChildAuthority::capture(
        runtime,
        FleetRole::Worker,
        "child-creator-gate".into(),
        "work".into(),
        None,
    )
    .as_ref()
    .clone();
    {
        let mut manager = captured.runtime.manager.write().await;
        captured.owner_agent_id = manager.insert_test_running_agent("creator-child", &f.workspace);
        manager.assign_test_session_owner(&captured.owner_agent_id, "origin-creator-gate");
    }
    let child_authority = Arc::new(captured);
    let job = crate::tools::subagent::engine::ChildJob::admitted(
        child_authority.clone(),
        crate::tools::subagent::SubAgentAssignment {
            objective: "attempt plugin install".into(),
            role: None,
            native_preset: None,
        },
        Instant::now(),
        4,
        false,
        None,
    )
    .await
    .expect("child job");
    let child_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-child-pm", "plugin_manager", &inside_args),
        canned::simple_text_turn("child done"),
    ]));
    let mut child_cfg = f.engine_config(f.discover());
    child_cfg.model = DEFAULT_TEXT_MODEL.into();
    child_cfg.max_steps = 4;
    let (mut child_engine, child_handle) = Engine::new_child_admitted(
        child_cfg,
        &api,
        child_authority,
        codewhale_models::SystemPrompt::Text("child".into()),
        None,
    )
    .expect("child engine");
    child_engine.model_client = Some(child_mock);
    child_engine
        .install_child_job(job, Vec::new())
        .expect("install child job");
    let child_spec = child_engine
        .child_turn_spec("child attempt".into(), None)
        .expect("child spec");
    child_handle
        .send(Op::SendMessage(child_spec))
        .await
        .expect("send child op");
    let collect = async {
        let mut events = child_handle.rx_event.write().await;
        let mut cards = 0usize;
        let mut results = Vec::new();
        while let Some(event) = events.recv().await {
            match event {
                Event::ApprovalRequired { .. } => cards += 1,
                Event::ToolCallComplete { result, .. } => results.push(result),
                _ => {}
            }
        }
        (cards, results)
    };
    let (child_result, (child_cards, child_tool_results)) =
        tokio::time::timeout(model_turn_event_timeout(), async {
            tokio::join!(Box::pin(child_engine.run_child()), collect)
        })
        .await
        .expect("child run timeout");
    child_result.expect("child run completes");
    assert_eq!(child_cards, 0);
    assert_eq!(child_tool_results.len(), 1);
    assert!(child_tool_results[0].is_err());

    let nested_code = serde_json::json!({
        "code": format!(
            "return await tools.call('plugin_manager', {{ action: 'install_bundle', target: {} }});",
            serde_json::to_string(&src_dir.to_string_lossy().as_ref()).expect("json string")
        )
    })
    .to_string();
    let nested_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-nested", "execute_tools", &nested_code),
        canned::simple_text_turn("nested done"),
    ]));
    let mut nested_cfg = f.engine_config(f.discover());
    nested_cfg.features.enable(Feature::CodeMode);
    let (nested_handle, nested_task) = spawn_interactive(nested_cfg, nested_mock);
    let nested_outcome = drive_turn(
        &nested_handle,
        build_turn_op(
            "nested attempt",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(nested_handle, nested_task).await;
    assert!(nested_outcome.cards.is_empty());
    assert_eq!(nested_outcome.tool_results.len(), 1);
    let nested_res = nested_outcome.tool_results[0]
        .1
        .as_ref()
        .expect("execute_tools outer receipt");
    assert!(!nested_res.success, "{}", nested_res.content);
    assert!(
        nested_res
            .content
            .contains("changes plugins and is not available inside execute_tools programs"),
        "{}",
        nested_res.content
    );
    assert!(f.discover().get("blocked-skill").is_none());

    f.manager.shutdown().await;
}

#[tokio::test]
async fn grants_allow_rules_hooks_and_runtime_narrowing_cannot_bypass_creator_gate() {
    let Some(f) = GateFixture::new(
        "grants_allow_rules_hooks_and_runtime_narrowing_cannot_bypass_creator_gate",
    ) else {
        return;
    };
    let src_dir = f.workspace.join("plugins/hardened-skill");
    write_skill_plugin(&src_dir, "hardened-skill");
    let install_args = serde_json::json!({
        "action": "install_bundle",
        "target": src_dir.to_string_lossy(),
    })
    .to_string();
    let config = Config::default();

    for non_user in [
        ApprovalDecider::SessionRule,
        ApprovalDecider::Posture,
        ApprovalDecider::Host,
    ] {
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call-nonuser", "plugin_manager", &install_args),
            canned::simple_text_turn("refused nonuser"),
        ]));
        let (handle, task) = spawn_interactive(f.engine_config(f.discover()), mock);
        let outcome = drive_turn(
            &handle,
            build_turn_op(
                "install with grant",
                AppMode::Agent,
                ApprovalMode::Suggest,
                &config,
                UserInputProvenance::ExternalUser,
                None,
            ),
            ApprovalBehavior::ApproveBy(non_user),
        )
        .await;
        shutdown_engine(handle, task).await;
        assert_eq!(outcome.cards.len(), 1);
        assert_permission_denied(&outcome);
    }

    let allow_rule_engine = ExecPolicyEngine::with_rulesets(vec![
        Ruleset::user(vec![], vec![]).with_ask_rules(vec![ToolAskRule {
            tool: "plugin_manager".to_string(),
            command: None,
            command_exact: false,
            path: None,
            workspace: None,
            action: PermissionAction::Allow,
        }]),
    ]);
    let allow_cmd = if cfg!(windows) {
        r#"echo {"decision":"allow"}"#
    } else {
        r#"echo '{"decision":"allow"}'"#
    };
    let allow_hooks = Arc::new(HookExecutor::new(
        HooksConfig {
            enabled: true,
            hooks: vec![Hook::new(HookEvent::ToolCallBefore, allow_cmd)],
            ..HooksConfig::default()
        },
        f.workspace.clone(),
    ));
    let mut rule_cfg = f.engine_config(f.discover());
    rule_cfg.exec_policy_engine = allow_rule_engine;
    let rule_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-rule-hook", "plugin_manager", &install_args),
        canned::simple_text_turn("card still shown"),
    ]));
    let (rule_handle, rule_task) = spawn_interactive(rule_cfg, rule_mock);
    let rule_outcome = drive_turn(
        &rule_handle,
        build_turn_op(
            "install with allow rule and allow hook",
            AppMode::Agent,
            ApprovalMode::Suggest,
            &config,
            UserInputProvenance::ExternalUser,
            Some(allow_hooks),
        ),
        ApprovalBehavior::DenyUser,
    )
    .await;
    shutdown_engine(rule_handle, rule_task).await;
    assert_eq!(rule_outcome.cards.len(), 1);
    assert_creator_card(&rule_outcome.cards[0]);
    assert_permission_denied(&rule_outcome);

    for (narrow_mode, narrow_approval) in [
        (AppMode::Agent, ApprovalMode::Never),
        (AppMode::Plan, ApprovalMode::Suggest),
    ] {
        let narrow_mock = Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call-narrow", "plugin_manager", &install_args),
            canned::simple_text_turn("narrowed"),
        ]));
        let (narrow_handle, narrow_task) =
            spawn_interactive(f.engine_config(f.discover()), narrow_mock);
        let narrow_outcome = drive_turn_with_hook(
            &narrow_handle,
            build_turn_op(
                "install and narrow",
                AppMode::Agent,
                ApprovalMode::Suggest,
                &config,
                UserInputProvenance::ExternalUser,
                None,
            ),
            ApprovalBehavior::ApproveUser,
            |handle, _| {
                handle.publish_turn_authority(
                    narrow_mode,
                    true,
                    false,
                    false,
                    narrow_approval,
                    None,
                );
            },
        )
        .await;
        shutdown_engine(narrow_handle, narrow_task).await;
        assert_eq!(narrow_outcome.cards.len(), 1);
        assert_permission_denied(&narrow_outcome);
    }

    assert!(f.discover().get("hardened-skill").is_none());
    f.manager.shutdown().await;
}

#[tokio::test]
async fn acp_surface_creator_gate_prompts_outside_scope_and_records_posture() {
    let Some(f) =
        GateFixture::new("acp_surface_creator_gate_prompts_outside_scope_and_records_posture")
    else {
        return;
    };
    let inside_dir = f.workspace.join("plugins/acp-inside");
    write_skill_plugin(&inside_dir, "acp-inside");
    let outside_dir = f.temp.path().join("outside/acp-outside");
    write_skill_plugin(&outside_dir, "acp-outside");
    let config = Config::default();

    let spawn_acp = |registry: PluginRegistry, mock: Arc<MockLlmClient>| {
        let (mut engine, mut handle) =
            Engine::new_with_model_client(f.engine_config(registry), &config, mock);
        engine.host_profile = EngineHostProfile::Acp;
        handle.set_host_profile(EngineHostProfile::Acp);
        let task = tokio::spawn(engine.run());
        (handle, task)
    };

    let bypass_in_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-acp-in",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": inside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("acp inside installed"),
    ]));
    let (in_handle, in_task) = spawn_acp(f.discover(), bypass_in_mock);
    let in_outcome = drive_turn(
        &in_handle,
        build_turn_op(
            "acp bypass inside",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(in_handle, in_task).await;
    assert!(in_outcome.cards.is_empty());
    assert!(in_outcome.tool_results[0].1.as_ref().expect("ok").success);
    let in_entry = read_state_entry(&f.state_path, "acp-inside").expect("acp-inside entry");
    assert!(matches!(
        parse_approver(in_entry["trust"].get("approved_by")),
        Some(PluginApprover::Agent(AgentApproval {
            posture: AgentPosture::FullAccess,
            ..
        }))
    ));

    let bypass_out_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-acp-out",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": outside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("acp outside installed"),
    ]));
    let (out_handle, out_task) = spawn_acp(f.discover(), bypass_out_mock);
    let out_outcome = drive_turn(
        &out_handle,
        build_turn_op(
            "acp bypass outside",
            AppMode::Agent,
            ApprovalMode::Bypass,
            &config,
            UserInputProvenance::ExternalUser,
            None,
        ),
        ApprovalBehavior::ApproveUser,
    )
    .await;
    shutdown_engine(out_handle, out_task).await;
    assert_eq!(out_outcome.cards.len(), 1);
    assert_creator_card(&out_outcome.cards[0]);
    assert!(out_outcome.tool_results[0].1.as_ref().expect("ok").success);
    let out_entry = read_state_entry(&f.state_path, "acp-outside").expect("acp-outside entry");
    assert_eq!(
        parse_approver(out_entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    let ar_out_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-acp-ar-out",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": outside_dir.to_string_lossy(),
            })
            .to_string(),
        ),
        canned::simple_text_turn("acp ar refused"),
    ]));
    let (ar_handle, ar_task) = spawn_acp(f.discover(), ar_out_mock.clone());
    let ar_outcome = drive_turn(
        &ar_handle,
        auto_review_message_op("acp auto review outside", &config),
        ApprovalBehavior::PanicOnCard,
    )
    .await;
    shutdown_engine(ar_handle, ar_task).await;
    assert!(ar_outcome.cards.is_empty());
    assert_permission_denied(&ar_outcome);
    assert_eq!(ar_out_mock.captured_requests().len(), 2);

    f.manager.shutdown().await;
}
