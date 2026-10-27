//! `core/call`: tickets, refusals, approval rules and the real host.
//!
//! The Node-backed tests drive a real host through a real extension tool
//! (`tests/fixtures/extension_host/core-call`) with a stand-in for the turn
//! loop's gate: it serves `NestedCallRequest`s exactly as the engine does and
//! can hold a request the way an approval card does. What the engine itself
//! decides (planning, the card, forced prompts, withdrawal) is tested in
//! `core::engine::approval::tests`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use codewhale_workflow_js::ToolCallResponse;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use super::core_call::{
    EXT_AUTO_ELIGIBLE, MAX_CALLS_PER_INVOCATION, OriginApproval, origin_approval, refusal,
    wire_from_response,
};
use super::protocol::{ContentBlockWire, CoreCallParams, RpcErrorWire, error_code};
use super::supervisor::HostRequestContext;
use super::tests::{FixturePlugins, fake_authority, node_for_tests};
use super::tier::HostTier;
use super::{ExtensionHostManager, ExtensionHostOptions, HostAttachment, SupervisionOptions};
use crate::plugins::activation::TestPolicyGuard;
use crate::tools::codemode::{
    ExtensionCaller, NestedCallGate, NestedCallRequest, NestedCallVerdict, NestedDecision,
};
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use codewhale_config::OwnerRef;

// ---------------------------------------------------------------------------
// A stand-in for the turn loop's gate
// ---------------------------------------------------------------------------

/// What the stand-in gate server saw. It serves the way the engine does: a
/// stale request is dropped unplanned, a held one waits for `release` or its
/// request's withdrawal (an approval card left open), anything else is run
/// exactly as asked.
#[derive(Default)]
struct Server {
    asked: AtomicUsize,
    stale: AtomicUsize,
    withdrawn: AtomicUsize,
    hold: AtomicBool,
    release: Notify,
}

fn serve(mut requests: mpsc::Receiver<NestedCallRequest>, server: Arc<Server>) {
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let request = match request {
                NestedCallRequest::Tool(request) => request,
                NestedCallRequest::UserQuestion { reply, .. } => {
                    let _ = reply.send(Err(ToolError::not_available(
                        "fixture has no user question backend",
                    )));
                    continue;
                }
                NestedCallRequest::Checkpoint { reply, .. } => {
                    let _ = reply.send(Err(ToolError::not_available(
                        "fixture has no session checkpoint backend",
                    )));
                    continue;
                }
            };
            if request.is_stale() {
                server.stale.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            server.asked.fetch_add(1, Ordering::SeqCst);
            let held = server.hold.load(Ordering::SeqCst);
            if held {
                match &request.withdraw {
                    Some(withdraw) => tokio::select! {
                        () = server.release.notified() => {}
                        () = withdraw.cancelled() => {
                            server.withdrawn.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                    },
                    None => server.release.notified().await,
                }
            }
            let _ = request.reply.send(NestedCallVerdict::Run {
                name: request.name.clone(),
                input: request.input.clone(),
                supports_parallel: true,
                decision: if held {
                    NestedDecision::Approved
                } else {
                    NestedDecision::Auto
                },
                hook_context: None,
            });
        }
    });
}

/// A tool that takes a while and records how many run at once.
struct SlowFixture {
    running: AtomicUsize,
    peak: AtomicUsize,
}

#[async_trait]
impl ToolSpec for SlowFixture {
    fn name(&self) -> &str {
        "slow_fixture"
    }
    fn description(&self) -> &str {
        "slow, read-only"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }
    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.running.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolResult::success("slow done"))
    }
}

/// An extension tool the core knows about, for the no-recursion rule.
struct FakeExtensionTool;

#[async_trait]
impl ToolSpec for FakeExtensionTool {
    fn name(&self) -> &str {
        "fake_ext"
    }
    fn description(&self) -> &str {
        "an extension tool"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode]
    }
    fn extension_caller(&self) -> Option<ExtensionCaller> {
        Some(ExtensionCaller {
            origin: "extension:fake".to_string(),
            tool: "fake_ext".to_string(),
            scope: "ext:fake@h".to_string(),
        })
    }
    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success("never"))
    }
}

/// One extension tool of the `core-call` fixture, running under the stand-in
/// gate with the file tools (and a slow one) as the tool snapshot its core
/// calls run against.
struct Rig {
    _manager: super::TestManagerGuard,
    tool: Arc<dyn ToolSpec>,
    context: ToolContext,
    server: Arc<Server>,
    slow: Arc<SlowFixture>,
}

impl Rig {
    fn new(engine: &HostAttachment, workspace: &Path, tool: &str) -> Self {
        let manager = super::TestManagerGuard::install(Arc::clone(&engine.manager));
        let slow = Arc::new(SlowFixture {
            running: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
            .with_file_tools()
            .build(ToolContext::new(workspace).with_plugin_registry(engine.plugin_view()));
        registry.register(slow.clone());
        engine.install_tools(&mut registry);
        let tool = registry.get(tool).expect("the fixture tool is installed");
        let caller = tool.extension_caller().expect("an extension tool");
        let (tx_event, mut rx_event) = mpsc::channel(64);
        tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
        let (gate, requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
        let server = Arc::new(Server::default());
        serve(requests, Arc::clone(&server));
        let mut context = ToolContext::new(workspace).with_plugin_registry(engine.plugin_view());
        context.execution.nested_call_gate = Some(gate.for_extension(caller, registry.all()));
        Self {
            _manager: manager,
            tool,
            context,
            server,
            slow,
        }
    }

    async fn run(&self, input: Value) -> Result<ToolResult, ToolError> {
        self.tool.execute(input, &self.context).await
    }

    async fn json(&self, input: Value) -> Value {
        let result = self.run(input).await.unwrap_or_else(|e| panic!("{e:?}"));
        serde_json::from_str(&result.content).expect("the tool answers JSON")
    }
}

async fn started(
    node: std::path::PathBuf,
    deadline: Option<Duration>,
) -> (FixturePlugins, Arc<ExtensionHostManager>, HostAttachment) {
    let fixture = FixturePlugins::new(&["core-call"]).await;
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
        node_override: Some(node),
        root: Some(fixture.root.clone()),
        supervision: SupervisionOptions {
            tool_call_deadline: deadline.unwrap_or(super::tool::TOOL_CALL_DEADLINE),
            ..Default::default()
        },
        ..Default::default()
    }));
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    (fixture, manager, engine)
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..300 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------
// Through the real host
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_extension_tool_runs_a_core_tool_through_the_gate_and_the_result_records_what_ran() {
    let Some(node) = node_for_tests("an_extension_tool_runs_a_core_tool") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let (fixture, manager, engine) = started(node, None).await;
    std::fs::write(
        fixture.workspace().join("note.txt"),
        "hello from the workspace",
    )
    .unwrap();
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");

    let result = rig
        .run(json!({"name": "read_file", "input": {"path": "note.txt"}}))
        .await
        .unwrap();
    let answer: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(answer["ok"]["isError"], false, "{answer}");
    assert!(
        answer["ok"]["content"]
            .as_str()
            .unwrap()
            .contains("hello from the workspace"),
        "{answer}"
    );
    assert_eq!(rig.server.asked.load(Ordering::SeqCst), 1);
    // The persisted record shows what the tool asked the core to run.
    let core_calls = &result.metadata.as_ref().unwrap()["core_calls"];
    assert_eq!(core_calls["total"], 1);
    assert_eq!(core_calls["calls"][0]["tool"], "read_file");
    assert_eq!(core_calls["calls"][0]["status"], "ok");
    assert_eq!(core_calls["calls"][0]["decision"], "auto");
    // A tool that made none leaves no `core_calls` key.
    let probe = Rig::new(&engine, fixture.workspace(), "cc_probe");
    let none = probe.run(json!({})).await.unwrap();
    assert!(none.metadata.as_ref().unwrap().get("core_calls").is_none());
    // The ticket does not outlive the call.
    assert_eq!(manager.shared.core_calls.live_tickets(), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn every_refusal_is_refused_through_the_real_host_before_the_gate_is_asked() {
    let Some(node) = node_for_tests("every_refusal_is_refused") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let (fixture, manager, engine) = started(node, None).await;
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");
    let names = [
        // Code mode's own, in the spellings the model could try.
        "execute_tools",
        "EXECUTE_TOOLS",
        "code_execution",
        "js_execution",
        "agent",
        "Agent",
        "workflow",
        "rlm",
        "request_user_input",
        "multi_tool_use.parallel",
        // MCP, Computer Use included.
        "mcp_demo_tool",
        "MCP_Demo_Tool",
        "mcp_computer_computer_register",
        "list_mcp_resources",
        "read_mcp_resource",
        // Discovery and retrieval.
        "tool_search",
        "Tool_Search_Tool_BM25",
        "tool_search_tool_regex",
        "retrieve_tool_result",
        // The memory writer and what changes the session's permissions.
        "remember",
        "REMEMBER",
        "request_plugin_install",
        "create_goal",
        "update_goal",
        "automation",
        "automation_create",
        "send_later",
        "start_mcp_server",
        "start_registry_mcp_server",
        // Extension tools, this one's own included.
        "cc_call",
        "CC_CALL",
        "cc_many",
    ];
    for name in names {
        let answer = rig.json(json!({"name": name, "input": {}})).await;
        assert_eq!(
            answer["failed"]["code"], "refused",
            "{name} must be refused: {answer}"
        );
    }
    // An interactive shell and a sandbox escalation, whatever the tool.
    for (name, input) in [
        ("bash", json!({"command": "ls", "interactive": true})),
        (
            "read_file",
            json!({"path": "x", "sandbox_permissions": "danger"}),
        ),
    ] {
        let answer = rig.json(json!({"name": name, "input": input})).await;
        assert_eq!(answer["failed"]["code"], "refused", "{name}: {answer}");
    }
    assert_eq!(
        rig.server.asked.load(Ordering::SeqCst),
        0,
        "no refused call reached planning"
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn without_the_turn_loops_gate_for_this_tool_there_is_no_ticket_and_no_core() {
    let Some(node) = node_for_tests("without_the_turn_loops_gate") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let (fixture, manager, engine) = started(node, None).await;
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");
    let input = json!({"name": "read_file", "input": {"path": "x"}});

    // No gate at all: a sub-agent, a test, a call nested in `execute_tools`.
    let bare = ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    let answer: Value = serde_json::from_str(
        &rig.tool
            .execute(input.clone(), &bare)
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(answer, json!({"noCore": true}));

    // A gate that is not an extension's (an `execute_tools` program's).
    let mut program =
        ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    program.execution.nested_call_gate = Some(NestedCallGate::admitting_for_test());
    let answer: Value = serde_json::from_str(
        &rig.tool
            .execute(input.clone(), &program)
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(answer, json!({"noCore": true}));

    // An extension gate served for another tool's call.
    let (tx_event, mut rx_event) = mpsc::channel(8);
    tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
    let (gate, requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
    let server = Arc::new(Server::default());
    serve(requests, Arc::clone(&server));
    let mut other =
        ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view());
    other.execution.nested_call_gate = Some(gate.for_extension(
        ExtensionCaller {
            origin: "extension:other".to_string(),
            tool: "other_tool".to_string(),
            scope: "ext:other@h".to_string(),
        },
        Vec::new(),
    ));
    let answer: Value =
        serde_json::from_str(&rig.tool.execute(input, &other).await.unwrap().content).unwrap();
    assert_eq!(answer, json!({"noCore": true}));
    assert_eq!(server.asked.load(Ordering::SeqCst), 0);

    // A command invocation never has `core`.
    let reference = manager
        .commands_for_plugins(&engine.plugin_view())
        .into_iter()
        .find(|entry| entry.registration.name == "cc-probe")
        .expect("the command is live")
        .reference();
    match super::command::run(&manager.shared, &reference, "", None).await {
        Ok(super::command::CommandOutcome::Show { text }) => {
            let probe: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(probe["hasCore"], false, "{text}");
        }
        other => panic!("{other:?}"),
    }
    manager.shutdown().await;
}

#[tokio::test]
async fn a_core_call_waiting_on_a_person_pauses_the_tool_call_deadline() {
    let Some(node) = node_for_tests("a_core_call_waiting_on_a_person") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let (fixture, manager, engine) = started(node, Some(Duration::from_secs(1))).await;
    std::fs::write(fixture.workspace().join("note.txt"), "slow approval").unwrap();
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");
    rig.server.hold.store(true, Ordering::SeqCst);

    // The approval takes 2.5 s: longer than the tool's whole 1 s deadline.
    let started_at = Instant::now();
    let release = async {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        rig.server.release.notify_one();
    };
    let (result, ()) = tokio::join!(
        rig.run(json!({"name": "read_file", "input": {"path": "note.txt"}})),
        release
    );
    let answer: Value =
        serde_json::from_str(&result.as_ref().expect("the call is not timed out").content).unwrap();
    assert_eq!(answer["ok"]["isError"], false, "{answer}");
    assert!(started_at.elapsed() >= Duration::from_millis(2400));
    assert_eq!(
        result.as_ref().unwrap().metadata.as_ref().unwrap()["core_calls"]["calls"][0]["decision"],
        "approved",
        "the receipt records that a person allowed it"
    );

    // The deadline still holds when nothing is paused.
    rig.server.hold.store(false, Ordering::SeqCst);
    let slow = Rig::new(&engine, fixture.workspace(), "cc_then_sleep");
    let error = slow
        .run(json!({"name": "read_file", "input": {"path": "note.txt"}, "ms": 5000}))
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::Timeout { .. }), "{error:?}");
    manager.shutdown().await;
}

#[tokio::test]
async fn cancel_revoke_and_host_exit_withdraw_a_pending_core_call() {
    let Some(node) = node_for_tests("cancel_revoke_and_host_exit") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let (fixture, manager, engine) = started(node, None).await;
    let call = json!({"name": "read_file", "input": {"path": "x"}});

    // The turn is cancelled (the tool call's future is dropped) while a card
    // is open: the server's wait for the person is withdrawn.
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");
    rig.server.hold.store(true, Ordering::SeqCst);
    let wait = tokio::time::timeout(Duration::from_millis(500), rig.run(call.clone())).await;
    assert!(wait.is_err(), "the call is still waiting on the card");
    assert_eq!(rig.server.asked.load(Ordering::SeqCst), 1);
    until("the withdrawal", || {
        rig.server.withdrawn.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(manager.shared.core_calls.live_tickets(), 0);

    // The plugin is disabled while a card is open: the owner's revocation
    // withdraws it, and the call fails cancelled.
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");
    rig.server.hold.store(true, Ordering::SeqCst);
    let disable = async {
        until("the card to open", || {
            rig.server.asked.load(Ordering::SeqCst) == 1
        })
        .await;
        engine.set_plugins(fixture.disable("core-call"));
        engine.sync().await.unwrap();
    };
    let (result, ()) = tokio::join!(rig.run(call.clone()), disable);
    assert!(result.is_err(), "a revoked tool's call fails: {result:?}");
    until("the withdrawal", || {
        rig.server.withdrawn.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(manager.shared.core_calls.live_tickets(), 0);

    // The host exits while a card is open (the plugin is enabled again first).
    crate::plugins::discovery::discover_with_config(&fixture.config)
        .enable("core-call")
        .unwrap();
    engine.set_plugins(fixture.registry());
    engine.sync().await.unwrap();
    let rig = Rig::new(&engine, fixture.workspace(), "cc_call");
    rig.server.hold.store(true, Ordering::SeqCst);
    let pid = manager.host_pid().expect("a running host");
    let kill = async {
        until("the card to open", || {
            rig.server.asked.load(Ordering::SeqCst) == 1
        })
        .await;
        #[cfg(unix)]
        let status = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap();
        #[cfg(windows)]
        let status = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    };
    let (result, ()) = tokio::join!(rig.run(call), kill);
    assert!(result.is_err(), "a killed host fails the call: {result:?}");
    until("the withdrawal", || {
        rig.server.withdrawn.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(manager.shared.core_calls.live_tickets(), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn an_invocation_has_at_most_fifty_core_calls_and_four_at_once() {
    let Some(node) = node_for_tests("an_invocation_has_at_most_fifty") else {
        return;
    };
    let _policy = TestPolicyGuard::extension_host(true);
    let (fixture, manager, engine) = started(node, None).await;
    std::fs::write(fixture.workspace().join("note.txt"), "n").unwrap();

    // 52 calls in a row: the first 50 run, the last two are refused.
    let rig = Rig::new(&engine, fixture.workspace(), "cc_many");
    let answer = rig
        .json(json!({"name": "read_file", "input": {"path": "note.txt"}, "count": 52}))
        .await;
    let outcomes = answer["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 52);
    assert!(
        outcomes[..MAX_CALLS_PER_INVOCATION as usize]
            .iter()
            .all(|outcome| outcome["ok"]["isError"] == false)
    );
    for outcome in &outcomes[MAX_CALLS_PER_INVOCATION as usize..] {
        assert_eq!(outcome["failed"]["code"], "refused", "{outcome}");
        assert!(
            outcome["failed"]["message"]
                .as_str()
                .unwrap()
                .contains("limit"),
            "{outcome}"
        );
    }
    assert_eq!(rig.server.asked.load(Ordering::SeqCst), 50);

    // Eight at once of a tool that takes a while: all run, never more than four together.
    let rig = Rig::new(&engine, fixture.workspace(), "cc_many");
    let answer = rig
        .json(json!({"name": "slow_fixture", "input": {}, "count": 8, "parallel": true}))
        .await;
    assert_eq!(answer["outcomes"].as_array().unwrap().len(), 8);
    assert!(
        answer["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|outcome| outcome["ok"]["isError"] == false)
    );
    let peak = rig.slow.peak.load(Ordering::SeqCst);
    assert!((2..=4).contains(&peak), "peak concurrency {peak}");
    manager.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tickets presented to the core (no host process)
// ---------------------------------------------------------------------------

struct Fixture {
    manager: ExtensionHostManager,
    owner: OwnerRef,
    other: OwnerRef,
    context: ToolContext,
    gate: NestedCallGate,
}

fn fixture(uses_server: bool) -> Fixture {
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    let mut registry = manager.shared.registry.lock().unwrap();
    let begin = |registry: &mut super::registry::OwnerRegistry, id: &str| {
        let owner = registry
            .begin_owner(HostTier::Plugin, id, id, Some(fake_authority(id)), "hash")
            .unwrap();
        registry.mark_active(&owner);
        owner
    };
    let owner = begin(&mut registry, "a");
    let other = begin(&mut registry, "b");
    drop(registry);
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    std::mem::forget(dir);
    std::fs::write(workspace.join("x"), "x").unwrap();
    let specs = crate::tools::registry::ToolRegistryBuilder::new()
        .with_file_tools()
        .build(ToolContext::new(&workspace))
        .all();
    let (tx_event, mut rx_event) = mpsc::channel(8);
    tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
    let (gate, requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
    if uses_server {
        serve(requests, Arc::new(Server::default()));
    }
    Fixture {
        manager,
        owner,
        other,
        context: ToolContext::new(&workspace),
        gate: gate.for_extension(
            ExtensionCaller {
                origin: "extension:a".to_string(),
                tool: "a_tool".to_string(),
                scope: "ext:a@hash".to_string(),
            },
            specs,
        ),
    }
}

impl Fixture {
    fn caller(&self) -> ExtensionCaller {
        self.gate.extension().unwrap().0.clone()
    }

    fn begin(&self, generation: u64, owner: &OwnerRef) -> super::core_call::InvocationGuard {
        self.manager
            .shared
            .core_calls
            .begin(
                HostTier::Plugin,
                generation,
                owner,
                "call-1",
                &self.caller(),
                &self.context,
                &self.gate,
            )
            .expect("an invocation")
    }

    async fn serve(
        &self,
        tier: HostTier,
        generation: u64,
        owner: &OwnerRef,
        ticket: &str,
    ) -> (Result<Value, RpcErrorWire>, Vec<String>) {
        let (cx, mut violations, _cancel) = HostRequestContext::for_test(1);
        let result = self
            .manager
            .shared
            .core_calls
            .serve(
                &self.manager.shared,
                tier,
                generation,
                CoreCallParams {
                    owner: owner.clone(),
                    ticket: ticket.to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "x"}),
                },
                cx,
            )
            .await;
        let mut seen = Vec::new();
        while let Ok(violation) = violations.try_recv() {
            seen.push(violation);
        }
        (result, seen)
    }
}

fn refused(result: &Result<Value, RpcErrorWire>) -> bool {
    matches!(result, Err(error) if error.code == error_code::REFUSED)
}

#[tokio::test]
async fn a_spoofed_ticket_is_refused_whoever_presents_it_and_a_burst_ends_the_host() {
    let f = fixture(true);
    let guard = f.begin(7, &f.owner);
    let ticket = guard.ticket().to_string();

    // The honest presentation works.
    let (ok, violations) = f.serve(HostTier::Plugin, 7, &f.owner, &ticket).await;
    assert!(ok.is_ok(), "{ok:?}");
    assert!(violations.is_empty());

    // Another plugin in the same host presents it: wrong owner.
    let (result, _) = f.serve(HostTier::Plugin, 7, &f.other, &ticket).await;
    assert!(refused(&result), "{result:?}");
    // The right plugin with another activation's token.
    let stale_owner = OwnerRef {
        owner_token: "0".repeat(32),
        ..f.owner.clone()
    };
    let (result, _) = f.serve(HostTier::Plugin, 7, &stale_owner, &ticket).await;
    assert!(refused(&result), "{result:?}");
    // From the other tier's host.
    let (result, _) = f.serve(HostTier::Builtin, 7, &f.owner, &ticket).await;
    assert!(refused(&result), "{result:?}");
    // After a host generation bump (a restarted host).
    let (result, _) = f.serve(HostTier::Plugin, 8, &f.owner, &ticket).await;
    assert!(refused(&result), "{result:?}");
    // A made-up ticket.
    let (result, _) = f.serve(HostTier::Plugin, 7, &f.owner, "cwt.guess").await;
    assert!(refused(&result), "{result:?}");
    // The refusal says nothing about which field mismatched, and never the ticket.
    let message = &result.as_ref().unwrap_err().message;
    assert!(!message.contains("cwt."), "{message}");

    // The ticket is still good for its owner afterwards.
    let (ok, _) = f.serve(HostTier::Plugin, 7, &f.owner, &ticket).await;
    assert!(ok.is_ok(), "{ok:?}");

    // Eight invalid presentations are a burst: the host is ended, once it is.
    let (_, violations) = f.serve(HostTier::Plugin, 7, &f.other, &ticket).await;
    assert!(violations.is_empty(), "{violations:?}");
    let mut ended = Vec::new();
    for _ in 0..8 {
        let (_, violations) = f.serve(HostTier::Plugin, 7, &f.other, "cwt.guess").await;
        ended.extend(violations);
    }
    assert!(
        ended
            .iter()
            .any(|reason| reason
                .starts_with("protocol violation: too many invalid core/call tickets")),
        "{ended:?}"
    );

    // The ticket dies with its host, its owner and its invocation.
    f.manager.shared.core_calls.revoke_host(HostTier::Plugin, 7);
    let (result, _) = f.serve(HostTier::Plugin, 7, &f.owner, &ticket).await;
    assert!(refused(&result), "after the host exited: {result:?}");
    let guard = f.begin(7, &f.owner);
    let second = guard.ticket().to_string();
    f.manager.shared.core_calls.revoke_owner("a");
    let (result, _) = f.serve(HostTier::Plugin, 7, &f.owner, &second).await;
    assert!(refused(&result), "after the owner was revoked: {result:?}");
    let guard = f.begin(7, &f.owner);
    let third = guard.ticket().to_string();
    drop(guard);
    let (result, _) = f.serve(HostTier::Plugin, 7, &f.owner, &third).await;
    assert!(refused(&result), "after the invocation ended: {result:?}");
    assert_eq!(f.manager.shared.core_calls.live_tickets(), 0);
}

#[tokio::test]
async fn a_ticket_for_a_revoked_owner_is_refused_even_if_it_was_never_revoked() {
    let mut f = fixture(true);
    let incomplete = f.begin(7, &f.owner);
    assert!(incomplete.checkpoint_ticket().is_none());
    assert!(incomplete.question_ticket().is_none());
    drop(incomplete);
    f.context = f
        .context
        .clone()
        .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
            "checkpoint-session".into(),
            "fixture-model".into(),
            f.context.workspace.clone(),
            None,
            Arc::new(Vec::new()),
        ))
        .with_origin_turn_id("checkpoint-turn");
    let guard = f.begin(7, &f.owner);
    let checkpoint_ticket = guard.checkpoint_ticket().unwrap().to_string();
    let question_ticket = guard.question_ticket().unwrap().to_string();
    let questions = vec![crate::tools::user_input::UserInputQuestion {
        id: "question".into(),
        question: "Choose or write".into(),
        header: String::new(),
        detail: None,
        intent: None,
        options: Vec::new(),
        allow_free_text: false,
        multi_select: false,
    }];
    let (cx, _violations, _cancel) = HostRequestContext::for_test(20);
    let unavailable = f
        .manager
        .shared
        .core_calls
        .serve_question(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::UserQuestionParams {
                owner: f.owner.clone(),
                ticket: question_ticket.clone(),
                questions: questions.clone(),
            },
            cx,
        )
        .await
        .unwrap_err();
    assert_eq!(unavailable.code, error_code::NOT_AVAILABLE);
    assert!(
        unavailable
            .message
            .contains("fixture has no user question backend")
    );
    let (wrong_method, _) = f
        .serve(HostTier::Plugin, 7, &f.owner, &question_ticket)
        .await;
    assert!(refused(&wrong_method));
    for (tier, generation, owner, ticket) in [
        (
            HostTier::Plugin,
            7,
            f.owner.clone(),
            guard.ticket().to_string(),
        ),
        (
            HostTier::Plugin,
            7,
            f.owner.clone(),
            checkpoint_ticket.clone(),
        ),
        (
            HostTier::Plugin,
            7,
            f.other.clone(),
            question_ticket.clone(),
        ),
        (
            HostTier::Builtin,
            7,
            f.owner.clone(),
            question_ticket.clone(),
        ),
        (
            HostTier::Plugin,
            8,
            f.owner.clone(),
            question_ticket.clone(),
        ),
    ] {
        let (cx, _violations, _cancel) = HostRequestContext::for_test(21);
        let result = f
            .manager
            .shared
            .core_calls
            .serve_question(
                &f.manager.shared,
                tier,
                generation,
                super::protocol::UserQuestionParams {
                    owner,
                    ticket,
                    questions: questions.clone(),
                },
                cx,
            )
            .await;
        assert!(refused(&result), "{result:?}");
    }
    let (cx, _violations, _cancel) = HostRequestContext::for_test(22);
    let malformed = f
        .manager
        .shared
        .core_calls
        .serve_question(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::UserQuestionParams {
                owner: f.owner.clone(),
                ticket: question_ticket.clone(),
                questions: Vec::new(),
            },
            cx,
        )
        .await
        .unwrap_err();
    assert_eq!(malformed.code, error_code::INVALID_PARAMS);
    let (cx, _violations, _cancel) = HostRequestContext::for_test(2);
    let unavailable = f
        .manager
        .shared
        .core_calls
        .serve_checkpoint(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::SessionCheckpointParams {
                owner: f.owner.clone(),
                ticket: checkpoint_ticket.clone(),
            },
            cx,
        )
        .await
        .unwrap_err();
    assert_eq!(unavailable.code, error_code::NOT_AVAILABLE);
    assert!(
        unavailable
            .message
            .contains("fixture has no session checkpoint backend")
    );
    let (wrong_method, _) = f
        .serve(HostTier::Plugin, 7, &f.owner, &checkpoint_ticket)
        .await;
    assert!(refused(&wrong_method));
    for (tier, generation, owner, ticket) in [
        (
            HostTier::Plugin,
            7,
            f.owner.clone(),
            guard.ticket().to_string(),
        ),
        (
            HostTier::Plugin,
            7,
            f.owner.clone(),
            question_ticket.clone(),
        ),
        (
            HostTier::Plugin,
            7,
            f.other.clone(),
            checkpoint_ticket.clone(),
        ),
        (
            HostTier::Builtin,
            7,
            f.owner.clone(),
            checkpoint_ticket.clone(),
        ),
        (
            HostTier::Plugin,
            8,
            f.owner.clone(),
            checkpoint_ticket.clone(),
        ),
    ] {
        let (cx, _violations, _cancel) = HostRequestContext::for_test(3);
        let result = f
            .manager
            .shared
            .core_calls
            .serve_checkpoint(
                &f.manager.shared,
                tier,
                generation,
                super::protocol::SessionCheckpointParams { owner, ticket },
                cx,
            )
            .await;
        assert!(refused(&result), "{result:?}");
    }
    let (caller, specs) = f.gate.extension().unwrap();
    let caller = caller.clone();
    let specs = specs.to_vec();
    let (tx, _events) = mpsc::channel(1);
    let (gate, mut requests) = NestedCallGate::new(None, tx, Duration::from_secs(60));
    f.gate = gate.for_extension(caller, specs);
    let pending = f.begin(7, &f.owner);
    let (cx, _violations, cancel) = HostRequestContext::for_test(4);
    let checkpoint = f.manager.shared.core_calls.serve_checkpoint(
        &f.manager.shared,
        HostTier::Plugin,
        7,
        super::protocol::SessionCheckpointParams {
            owner: f.owner.clone(),
            ticket: pending.checkpoint_ticket().unwrap().to_string(),
        },
        cx,
    );
    tokio::pin!(checkpoint);
    let request = tokio::select! {
        result = &mut checkpoint => panic!("checkpoint unexpectedly settled: {result:?}"),
        request = requests.recv() => request.unwrap(),
    };
    cancel.cancel();
    assert_eq!(checkpoint.await.unwrap_err().code, error_code::CANCELLED);
    let NestedCallRequest::Checkpoint {
        caller,
        withdraw,
        reply,
    } = request
    else {
        panic!("expected typed checkpoint")
    };
    assert_eq!(caller.session_id, "checkpoint-session");
    assert_eq!(caller.turn_id, "checkpoint-turn");
    assert!(withdraw.is_cancelled());
    assert!(reply.is_closed());
    drop(pending);
    {
        let pending = f.begin(7, &f.owner);
        let (cx, _violations, _cancel) = HostRequestContext::for_test(23);
        let question = f.manager.shared.core_calls.serve_question(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::UserQuestionParams {
                owner: f.owner.clone(),
                ticket: pending.question_ticket().unwrap().to_string(),
                questions: questions.clone(),
            },
            cx,
        );
        tokio::pin!(question);
        let request = tokio::select! {
            result = &mut question => panic!("question unexpectedly settled: {result:?}"),
            request = requests.recv() => request.unwrap(),
        };
        let NestedCallRequest::UserQuestion {
            caller,
            request,
            withdraw,
            reply,
            ..
        } = request
        else {
            panic!("expected typed question")
        };
        assert_eq!(caller.session_id, "checkpoint-session");
        assert_eq!(caller.turn_id, "checkpoint-turn");
        assert_eq!(caller.workspace, f.context.workspace);
        assert!(caller.agent_id.is_none());
        assert!(request.require_typed_answers);
        assert!(request.questions[0].allow_free_text);
        assert!(!withdraw.is_cancelled());
        let (cx, _violations, _cancel) = HostRequestContext::for_test(24);
        let checkpoint = f.manager.shared.core_calls.serve_checkpoint(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::SessionCheckpointParams {
                owner: f.owner.clone(),
                ticket: pending.checkpoint_ticket().unwrap().to_string(),
            },
            cx,
        );
        tokio::pin!(checkpoint);
        assert!(tokio::time::timeout(Duration::from_millis(2), async {
            tokio::select! {
                result = &mut checkpoint => panic!("checkpoint passed an unanswered question: {result:?}"),
                request = requests.recv() => panic!("parallel session request: {}", request.is_some()),
            }
        }).await.is_err());
        let response = crate::tools::user_input::UserInputResponse {
            answers: vec![crate::tools::user_input::UserInputAnswer {
                kind: Some(crate::tools::user_input::UserInputAnswerKind::Custom),
                id: "question".into(),
                label: String::new(),
                value: "exact, custom Other".into(),
            }],
        };
        request.validate_response(&response).unwrap();
        let expected = serde_json::to_value(&response).unwrap();
        reply.send(Ok(response)).unwrap();
        assert_eq!(question.await.unwrap(), expected);
        let request = tokio::select! {
            result = &mut checkpoint => panic!("checkpoint unexpectedly settled: {result:?}"),
            request = requests.recv() => request.unwrap(),
        };
        let NestedCallRequest::Checkpoint { reply, .. } = request else {
            panic!("expected serialized checkpoint")
        };
        reply.send(Ok(true)).unwrap();
        assert_eq!(checkpoint.await.unwrap(), Value::Bool(true));
    }
    for (error, expected_code, expected_data) in [
        (
            ToolError::not_available("backend closed"),
            error_code::NOT_AVAILABLE,
            None,
        ),
        (
            ToolError::execution_failed_with_metadata(
                "no responder",
                json!({"user_question_error": "NO_PROVIDER", "internal": "not forwarded"}),
            ),
            error_code::NOT_AVAILABLE,
            Some(json!({"user_question_error": "NO_PROVIDER"})),
        ),
        (
            ToolError::execution_failed_with_metadata(
                "other failure",
                json!({"user_question_error": "OTHER"}),
            ),
            error_code::NOT_AVAILABLE,
            None,
        ),
        (
            ToolError::execution_failed_with_metadata(
                "human cancelled",
                json!({"user_question_error": "ASK_CANCELLED", "internal": "not forwarded"}),
            ),
            error_code::CANCELLED,
            Some(json!({"user_question_error": "ASK_CANCELLED"})),
        ),
        (
            ToolError::cancelled("human cancelled"),
            error_code::CANCELLED,
            None,
        ),
    ] {
        let pending = f.begin(7, &f.owner);
        let (cx, _violations, _cancel) = HostRequestContext::for_test(26);
        let question = f.manager.shared.core_calls.serve_question(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::UserQuestionParams {
                owner: f.owner.clone(),
                ticket: pending.question_ticket().unwrap().to_string(),
                questions: questions.clone(),
            },
            cx,
        );
        tokio::pin!(question);
        let request = tokio::select! {
            result = &mut question => panic!("question unexpectedly settled: {result:?}"),
            request = requests.recv() => request.unwrap(),
        };
        let NestedCallRequest::UserQuestion { reply, .. } = request else {
            panic!("expected typed question")
        };
        reply.send(Err(error)).unwrap();
        let result = question.await.unwrap_err();
        assert_eq!(result.code, expected_code);
        assert_eq!(result.data, expected_data);
    }
    for cancellation in ["host", "invocation", "revocation"] {
        let mut pending = Some(f.begin(7, &f.owner));
        let ticket = pending
            .as_ref()
            .unwrap()
            .question_ticket()
            .unwrap()
            .to_string();
        let (cx, _violations, cancel) = HostRequestContext::for_test(25);
        let question = f.manager.shared.core_calls.serve_question(
            &f.manager.shared,
            HostTier::Plugin,
            7,
            super::protocol::UserQuestionParams {
                owner: f.owner.clone(),
                ticket,
                questions: questions.clone(),
            },
            cx,
        );
        tokio::pin!(question);
        let request = tokio::select! {
            result = &mut question => panic!("question unexpectedly settled: {result:?}"),
            request = requests.recv() => request.unwrap(),
        };
        match cancellation {
            "host" => cancel.cancel(),
            "invocation" => drop(pending.take()),
            "revocation" => f.manager.shared.core_calls.revoke_host(HostTier::Plugin, 7),
            _ => unreachable!(),
        }
        assert_eq!(question.await.unwrap_err().code, error_code::CANCELLED);
        let NestedCallRequest::UserQuestion {
            withdraw, reply, ..
        } = request
        else {
            panic!("expected typed question")
        };
        assert!(withdraw.is_cancelled());
        assert!(reply.is_closed());
        drop(pending);
    }
    assert_eq!(f.manager.shared.core_calls.live_tickets(), 0);
    let mut child_context = f.context.clone();
    child_context.execution.owner_agent_id = Some("child".into());
    let child = f
        .manager
        .shared
        .core_calls
        .begin(
            HostTier::Plugin,
            7,
            &f.owner,
            "child-call",
            &f.caller(),
            &child_context,
            &f.gate,
        )
        .unwrap();
    assert!(child.checkpoint_ticket().is_none());
    assert!(child.question_ticket().is_none());
    drop(child);
    drop(guard);
    let guard = f.begin(7, &f.owner);
    let checkpoint_ticket = guard.checkpoint_ticket().unwrap().to_string();
    // The owner is replaced by a new activation: its token changes, so the
    // old one the host still holds no longer names the current owner.
    let ticket = guard.ticket().to_string();
    f.manager.shared.registry.lock().unwrap().revoke_owner("a");
    let (result, _) = f.serve(HostTier::Plugin, 7, &f.owner, &ticket).await;
    assert!(refused(&result), "{result:?}");
    let (cx, _violations, _cancel) = HostRequestContext::for_test(5);
    assert!(refused(
        &f.manager
            .shared
            .core_calls
            .serve_checkpoint(
                &f.manager.shared,
                HostTier::Plugin,
                7,
                super::protocol::SessionCheckpointParams {
                    owner: f.owner.clone(),
                    ticket: checkpoint_ticket
                },
                cx,
            )
            .await
    ));
    drop(guard);
    assert_eq!(f.manager.shared.core_calls.live_tickets(), 0);
}

#[test]
fn the_wire_result_is_the_tools_text_and_json_and_says_when_it_was_cut() {
    use crate::core::events::{
        SessionCheckpointCaller, SessionCheckpointProjection, SessionCheckpointReceipt,
        SessionCheckpointRequest,
    };
    let snapshot = Arc::new(crate::core::ops::SessionSnapshot {
        session_id: "session".into(),
        messages: Vec::new(),
        total_tokens: 0,
        model: "fixture".into(),
        model_provider: "fixture".into(),
        model_provider_id: None,
        workspace: "/workspace".into(),
        system_prompt: None,
        instruction_assembly: None,
        mode: "agent".into(),
    });
    let hash = SessionCheckpointRequest::prefix_hash(&snapshot.messages).unwrap();
    let make = |projection| {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        (
            SessionCheckpointRequest::new(
                SessionCheckpointCaller {
                    session_id: "session".into(),
                    agent_id: None,
                    turn_id: "turn".into(),
                    call_id: "call".into(),
                    workspace: "/workspace".into(),
                },
                snapshot.clone(),
                hash.clone(),
                projection,
                tokio_util::sync::CancellationToken::new(),
                reply,
            ),
            receiver,
        )
    };
    let (request, mut answer) = make(SessionCheckpointProjection::Exact);
    request
        .validate_prefix("session", Path::new("/workspace"), &snapshot.messages)
        .unwrap();
    assert!(
        request
            .validate_prefix("other", Path::new("/workspace"), &snapshot.messages)
            .is_err()
    );
    request.succeed(SessionCheckpointReceipt {
        persisted_sha256: hash.clone(),
        messages_len: 0,
        projection: SessionCheckpointProjection::Exact,
    });
    assert!(answer.try_recv().unwrap().unwrap());
    assert!(request.is_stale());
    let (request, mut answer) = make(SessionCheckpointProjection::Exact);
    request.succeed(SessionCheckpointReceipt {
        persisted_sha256: "0".repeat(64),
        messages_len: 0,
        projection: SessionCheckpointProjection::Exact,
    });
    assert!(answer.try_recv().unwrap().is_err());
    let (request, mut answer) = make(SessionCheckpointProjection::Exact);
    request.succeed(SessionCheckpointReceipt {
        persisted_sha256: "0".repeat(64),
        messages_len: 0,
        projection: SessionCheckpointProjection::RuntimeReceiptRedacted,
    });
    assert!(answer.try_recv().unwrap().is_err());
    let (request, mut answer) = make(SessionCheckpointProjection::RuntimeReceiptRedacted);
    request.succeed(SessionCheckpointReceipt {
        persisted_sha256: "0".repeat(64),
        messages_len: 0,
        projection: SessionCheckpointProjection::RuntimeReceiptRedacted,
    });
    assert!(answer.try_recv().unwrap().unwrap());
    let (request, mut answer) = make(SessionCheckpointProjection::Exact);
    request.withdraw.cancel();
    request.succeed(SessionCheckpointReceipt {
        persisted_sha256: hash.clone(),
        messages_len: 0,
        projection: SessionCheckpointProjection::Exact,
    });
    assert!(answer.try_recv().unwrap().is_err());
    let (request, mut answer) = make(SessionCheckpointProjection::Exact);
    request.no_backend();
    assert!(!answer.try_recv().unwrap().unwrap());
    let (request, mut answer) = make(SessionCheckpointProjection::Exact);
    drop(request);
    assert!(
        answer
            .try_recv()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("did not acknowledge")
    );
    let text = |wire: &super::protocol::ToolResultWire| match &wire.content[..] {
        [ContentBlockWire::Text { text }] => text.clone(),
        other => panic!("{other:?}"),
    };
    let ok = wire_from_response(ToolCallResponse {
        ok: true,
        result: json!({"content": "plain", "metadata": null, "truncated": null}),
    });
    assert_eq!(
        (text(&ok).as_str(), ok.is_error, &ok.structured),
        ("plain", false, &None)
    );
    let json_content = wire_from_response(ToolCallResponse {
        ok: true,
        result: json!({"content": {"a": 1}, "metadata": null, "truncated": null}),
    });
    assert_eq!(text(&json_content), r#"{"a":1}"#);
    assert_eq!(json_content.structured, Some(json!({"a": 1})));
    let cut = wire_from_response(ToolCallResponse {
        ok: true,
        result: json!({"content": "head", "metadata": null,
            "truncated": {"original_bytes": 9000, "kept_bytes": 4, "spill_path": "/secret/path"}}),
    });
    let cut_text = text(&cut);
    assert!(
        cut_text.contains("9000 bytes") && cut_text.contains("first 4"),
        "{cut_text}"
    );
    assert!(
        !cut_text.contains("/secret/path"),
        "no local path reaches the host"
    );
    let failed = wire_from_response(ToolCallResponse {
        ok: false,
        result: json!("tool failed"),
    });
    assert_eq!(
        (text(&failed).as_str(), failed.is_error),
        ("tool failed", true)
    );
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[test]
fn the_refusal_list_is_case_insensitive_covers_aliases_and_spares_ordinary_tools() {
    let specs: Vec<Arc<dyn ToolSpec>> = vec![Arc::new(FakeExtensionTool)];
    let empty = json!({});
    for name in [
        "execute_tools",
        "Execute_Tools",
        "EXECUTE_TOOLS",
        "agent",
        "AGENT",
        "mcp_a_b",
        "MCP_A_B",
        "tool_search",
        "TOOL_SEARCH_TOOL_BM25",
        "retrieve_tool_result",
        "Remember",
        "create_goal",
        "automation_create",
        "AUTOMATION",
        "request_plugin_install",
        "fake_ext",
        "FAKE_EXT",
    ] {
        assert!(refusal(&specs, name, &empty).is_some(), "{name}");
    }
    assert!(refusal(&specs, "bash", &json!({"interactive": true})).is_some());
    assert!(refusal(&specs, "read_file", &json!({"sandbox_permissions": "x"})).is_some());
    // Ordinary tools are not refused here: they are planned, and prompt.
    for name in [
        "read_file",
        "read",
        "write_file",
        "bash",
        "web_search",
        "fetch_url",
        "list_dir",
    ] {
        assert_eq!(refusal(&specs, name, &empty), None, "{name}");
    }
}

#[test]
fn the_auto_table_is_registered_read_only_tools_and_shell_and_network_force_a_prompt() {
    // Every row is a real, read-only, auto-approved tool.
    let registry = crate::tools::registry::ToolRegistryBuilder::new()
        .with_file_tools()
        .with_search_tools()
        .build(ToolContext::new(Path::new("/w")));
    for name in EXT_AUTO_ELIGIBLE {
        let spec = registry
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not a registered tool"));
        assert!(spec.is_read_only(), "{name}");
        assert_eq!(
            spec.approval_requirement(),
            ApprovalRequirement::Auto,
            "{name}"
        );
    }

    let input = json!({});
    for name in EXT_AUTO_ELIGIBLE {
        // Planning found nothing that asks: unchanged. If it did ask, still asks.
        assert_eq!(
            origin_approval(name, &input, false, None),
            OriginApproval::Unchanged,
            "{name}"
        );
        assert_eq!(
            origin_approval(name, &input, true, None),
            OriginApproval::Prompt,
            "{name}"
        );
    }
    // Anything else needs approval even where the model's call would not.
    for name in [
        "write_file",
        "edit_file",
        "apply_patch",
        "todo_write",
        "notify",
        "tui_help",
    ] {
        assert_eq!(
            origin_approval(name, &input, false, None),
            OriginApproval::Prompt,
            "{name}"
        );
    }
    // Shell and network force a prompt, in every spelling.
    for name in [
        "bash",
        "Bash",
        "BASH",
        "exec_shell",
        "task_shell_start",
        "web_search",
        "fetch_url",
        "web.run",
        "WEB.RUN",
        "git_fetch",
        "run_tests",
        "verify",
        "finance",
    ] {
        assert_eq!(
            origin_approval(name, &input, false, None),
            OriginApproval::ForcePrompt,
            "{name}"
        );
        assert_eq!(
            origin_approval(name, &input, true, None),
            OriginApproval::ForcePrompt,
            "{name}"
        );
    }
}

#[test]
fn action_families_and_registered_process_or_network_tools_always_force_a_prompt() {
    let registry = crate::tools::registry::ToolRegistryBuilder::new()
        .with_file_tools()
        .with_git_tools()
        .with_test_runner_tool()
        .with_runtime_task_tools()
        .with_web_tools()
        .build(ToolContext::new(Path::new("/w")));
    for (name, action) in [
        ("Git", "fetch"),
        ("Run", "tests"),
        ("Web", "search"),
        ("Web", "fetch"),
        ("tasks", "gate_run"),
        ("github", "issue_context"),
    ] {
        let input = json!({"action": action});
        let spec = registry.get(name).expect("registered family");
        for spelling in [
            name.to_string(),
            name.to_ascii_lowercase(),
            name.to_ascii_uppercase(),
        ] {
            assert_eq!(
                origin_approval(&spelling, &input, false, Some(spec.as_ref())),
                OriginApproval::ForcePrompt,
                "{spelling}/{action} must prompt even under Full Access or a grant"
            );
        }
    }
    // Resolve family semantics even for core meta-tools without a registry spec.
    for (name, action) in [("gIt", "fetch"), ("rUn", "tests"), ("wEb", "search")] {
        assert_eq!(
            origin_approval(name, &json!({"action": action}), false, None),
            OriginApproval::ForcePrompt,
            "{name}/{action}"
        );
    }
    // The same family preserves safe workspace reads.
    let file = registry.get("File").expect("registered File family");
    assert_eq!(
        origin_approval(
            "File",
            &json!({"action": "read", "path": "note.txt"}),
            false,
            Some(file.as_ref())
        ),
        OriginApproval::Unchanged
    );
}

/// How each existing approval posture resolves what an extension's call needs
/// (the table in `docs/EXTENSIONS.md`): forced extension calls ask in both
/// Ask and Full Access; Auto-Review and Never refuse them. Full Access
/// auto-approves ordinary extension calls as it does for the model.
#[test]
fn extension_calls_resolve_against_every_posture_as_documented() {
    use crate::core::authority::{
        ApprovalRequestDisposition as D, TurnAuthority, resolve_approval_request_disposition,
    };
    use codewhale_config::AppMode;
    use codewhale_execpolicy::ApprovalMode;

    let authority = |auto_approve: bool, mode: ApprovalMode| {
        TurnAuthority::from_effective_fields(AppMode::Agent, true, false, auto_approve, mode)
    };
    let input = json!({});
    // (posture, session grant held for exactly this extension's call)
    let ask = authority(false, ApprovalMode::Suggest);
    let full = authority(true, ApprovalMode::Bypass);
    let auto_review = authority(false, ApprovalMode::Auto);
    let never = authority(false, ApprovalMode::Never);

    let resolve = |authority: &TurnAuthority, name: &str, granted: bool| {
        let approval = origin_approval(name, &input, false, None);
        match approval {
            OriginApproval::Unchanged => None,
            OriginApproval::Prompt => Some(resolve_approval_request_disposition(
                authority, granted, false, false, true,
            )),
            OriginApproval::ForcePrompt => Some(resolve_approval_request_disposition(
                authority, granted, false, true, true,
            )),
        }
    };

    // A read-only workspace tool: no request at all, in any posture.
    for authority in [&ask, &full, &auto_review, &never] {
        assert_eq!(resolve(authority, "read_file", false), None);
    }
    // An ordinary tool that needs approval ("write_file").
    assert_eq!(resolve(&ask, "write_file", false), Some(D::Prompt));
    assert_eq!(
        resolve(&ask, "write_file", true),
        Some(D::AutoApprove),
        "an extension-scoped grant"
    );
    assert_eq!(resolve(&full, "write_file", false), Some(D::AutoApprove));
    assert_eq!(
        resolve(&auto_review, "write_file", false),
        Some(D::AutoDenyAutoReview)
    );
    assert_eq!(
        resolve(&never, "write_file", true),
        Some(D::AutoDenyNeverPosture)
    );
    // Shell and network ask in Ask and Full Access, whatever grant exists.
    // Explicit no-prompt postures still refuse.
    for name in ["bash", "web_search"] {
        assert_eq!(resolve(&ask, name, false), Some(D::Prompt), "{name}");
        assert_eq!(
            resolve(&ask, name, true),
            Some(D::Prompt),
            "{name}: a grant never satisfies it"
        );
        assert_eq!(resolve(&full, name, false), Some(D::Prompt), "{name}");
        assert_eq!(resolve(&full, name, true), Some(D::Prompt), "{name}");
        assert_eq!(
            resolve(&auto_review, name, false),
            Some(D::AutoDenyAutoReview),
            "{name}"
        );
        assert_eq!(
            resolve(&never, name, true),
            Some(D::AutoDenyNeverPosture),
            "{name}"
        );
    }
}

#[test]
fn approval_keys_are_scoped_to_the_extension_so_grants_never_cross_origins() {
    use crate::tools::approval_cache::{approval_keys_for_call, extension_origin_approval_keys};
    let input = json!({"command": "ls -la"});
    let model = approval_keys_for_call(None, "bash", &input);
    let ext = extension_origin_approval_keys("ext:a@h1", None, "bash", &input);
    let other_build = extension_origin_approval_keys("ext:a@h2", None, "bash", &input);
    let other_plugin = extension_origin_approval_keys("ext:b@h1", None, "bash", &input);
    for (one, two) in [(&model, &ext), (&ext, &other_build), (&ext, &other_plugin)] {
        assert_ne!(one.0, two.0, "exact keys must differ");
        assert_ne!(one.1, two.1, "grouping keys must differ");
    }
    assert!(ext.0.0.starts_with("extcall:ext:a@h1:") && ext.1.0.starts_with("extcall:ext:a@h1:"));
    // Both directions: a grant held for one origin is looked up by the other's key.
    let held_by_model: std::collections::HashSet<String> = [model.1.0.clone()].into();
    assert!(
        !held_by_model.contains(&ext.1.0),
        "a grant given to the model never covers an extension call"
    );
    let held_by_extension: std::collections::HashSet<String> = [ext.1.0.clone()].into();
    assert!(
        !held_by_extension.contains(&model.1.0),
        "a grant given to the extension never covers the model's call"
    );
}
