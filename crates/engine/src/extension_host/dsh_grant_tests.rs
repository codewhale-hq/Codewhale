use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::core_call::InvocationGuard;
use super::dsh_broker::{Arm, BrokerCall, BrokerFuture, BrokerMethods, BrokerRefusal, decode};
use super::protocol::{DshBrokerParams, DshService, HostRequest, RpcErrorWire, error_code};
use super::supervisor::HostRequestContext;
use super::tier::HostCohort;
use super::{ExtensionHostManager, ExtensionHostOptions};
use crate::tools::codemode::{ExtensionCaller, NestedCallGate};
use crate::tools::spec::ToolContext;
use codewhale_config::OwnerRef;
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::host_services::{
    HostService, PersonVia, PluginApprover, ReviewApproval, TestOffer,
};

const HOST: u64 = 7;

struct Plugins {
    _temp: tempfile::TempDir,
    config: DiscoveryConfig,
}

impl Plugins {
    fn new(plugins: &[(&str, &[&str])]) -> Self {
        let temp = tempfile::tempdir_in(crate::test_support::test_scratch_root()).unwrap();
        let root = temp.path();
        let config = DiscoveryConfig {
            workspace: root.join("project"),
            user_plugins_dir: root.join("user"),
            workspace_plugins_dir: root.join("workspace"),
            builtin_plugin_dirs: Vec::new(),
            state_path: root.join("state/plugin-state.json"),
        };
        std::fs::create_dir_all(&config.workspace).unwrap();
        for (name, asks) in plugins {
            let plugin = config.user_plugins_dir.join(name);
            std::fs::create_dir_all(plugin.join("native")).unwrap();
            std::fs::write(
                plugin.join("native/index.mjs"),
                "export function apply() {}\n",
            )
            .unwrap();
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
            std::fs::write(
                plugin.join("plugin.toml"),
                format!(
                    "schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n[native]\npath = \"native/index.mjs\"\n{asked}"
                ),
            )
            .unwrap();
        }
        let plugins_fixture = Self {
            _temp: temp,
            config,
        };
        for (name, _) in plugins {
            plugins_fixture.review(name, true);
        }
        plugins_fixture
    }

    fn registry(&self) -> PluginRegistry {
        discover_with_config(&self.config)
    }

    fn review(&self, name: &str, showing_services: bool) {
        let mut registry = self.registry();
        let shown = registry
            .host_service_review(name)
            .filter(|_| showing_services)
            .map(|review| review.token);
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
}

struct Host {
    manager: ExtensionHostManager,
    context: ToolContext,
    gate: NestedCallGate,
}

fn host(workspace: &Path) -> Host {
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    let (tx_event, mut rx_event) = mpsc::channel(8);
    tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
    let (gate, _requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
    let context = ToolContext::new(workspace)
        .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
            "grant-session".into(),
            "fixture-model".into(),
            workspace.to_path_buf(),
            None,
            Arc::new(Vec::new()),
        ))
        .with_origin_turn_id("grant-turn");
    Host {
        manager,
        context,
        gate: gate.for_extension(
            ExtensionCaller {
                origin: "extension:grant".to_string(),
                tool: "grant_tool".to_string(),
                scope: "ext:grant@hash".to_string(),
            },
            Vec::new(),
        ),
    }
}

impl Host {
    fn load(&self, plugins: &Plugins, name: &str) -> OwnerRef {
        let authority = plugins.registry().authority_for(name).unwrap();
        let services = codewhale_plugins::registry::granted_host_services(&authority)
            .unwrap()
            .into_iter()
            .filter_map(|service| DshService::parse(service.name()))
            .collect();
        let mut registry = self.manager.shared.registry.lock().unwrap();
        let owner = registry
            .begin_owner(
                HostCohort::reviewed(),
                authority.plugin_id.as_str(),
                name,
                Some(authority.clone()),
                &authority.content_hash,
            )
            .unwrap();
        registry.set_host_services(&owner, services);
        registry.mark_active(&owner);
        owner
    }

    fn mint(&self, cohort: HostCohort, owner: &OwnerRef) -> InvocationGuard {
        self.manager
            .shared
            .core_calls
            .begin_scoped(
                cohort,
                HOST,
                owner,
                "call-1",
                self.gate.extension().unwrap().0,
                &self.context,
                &self.gate,
                None,
                String::new(),
                &self.manager.shared.broker_services(owner),
            )
            .expect("an invocation")
    }

    fn services(&self, owner: &OwnerRef) -> Vec<&'static str> {
        self.mint(HostCohort::reviewed(), owner)
            .broker_tickets()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|wire| DshService::parse(&wire.service).map(DshService::name))
            .collect()
    }

    async fn call(
        &self,
        table: &[(DshService, Arm)],
        owner: &OwnerRef,
        ticket: &str,
        service: &str,
    ) -> (Result<Value, RpcErrorWire>, Vec<String>) {
        let (cx, mut violations, _cancel) = HostRequestContext::for_test(1);
        let result = self
            .manager
            .shared
            .core_calls
            .serve_broker_in(
                table,
                &self.manager.shared,
                HostCohort::reviewed(),
                HOST,
                DshBrokerParams {
                    owner: owner.clone(),
                    ticket: ticket.to_string(),
                    service: service.to_string(),
                    method: "echo".to_string(),
                    input: json!({ "text": "ok", "granted": ["llm"], "host_services": ["llm"] }),
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

fn ticket(guard: &InvocationGuard, service: &str) -> String {
    guard
        .broker_tickets()
        .unwrap_or_default()
        .into_iter()
        .find(|wire| wire.service == service)
        .map(|wire| wire.ticket)
        .unwrap_or_else(|| panic!("no {service} ticket"))
}

fn refusal(result: &Result<Value, RpcErrorWire>) -> (i64, String, String) {
    let error = result.as_ref().expect_err("a refusal");
    (
        error.code,
        error.data.as_ref().unwrap()["dsh_refusal"]
            .as_str()
            .unwrap()
            .to_string(),
        error.message.clone(),
    )
}

fn not_granted(service: &str) -> (i64, String, String) {
    (
        error_code::REFUSED,
        "not_granted".to_string(),
        format!("the {service} service is not granted to this plugin"),
    )
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", content = "input")]
enum Echo {
    #[serde(rename = "echo")]
    Echo { text: String },
}

impl BrokerMethods for Echo {
    const NAMES: &'static [&'static str] = &["echo"];
}

fn echo_arm(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        let Echo::Echo { text } = decode::<Echo>(&call)?;
        Ok(json!({ "plugin": call.owner.plugin_id, "service": call.service.name(), "text": text }))
    })
}

fn echo() -> Vec<(DshService, Arm)> {
    DshService::ALL
        .into_iter()
        .map(|service| (service, echo_arm as Arm))
        .collect()
}

static WAITING: AtomicBool = AtomicBool::new(false);
static WITHDRAWN: AtomicBool = AtomicBool::new(false);

fn waiting_arm(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        let cancel = call.context.cancel().clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            WITHDRAWN.store(true, Ordering::SeqCst);
        });
        WAITING.store(true, Ordering::SeqCst);
        std::future::pending::<()>().await;
        Err(BrokerRefusal::NotAvailable(call.service))
    })
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn the_engine_and_the_review_record_name_the_same_services() {
    assert_eq!(
        DshService::ALL.map(DshService::name),
        HostService::ALL.map(HostService::name)
    );
    for service in DshService::ALL {
        assert_eq!(
            super::dsh_broker::offered(service),
            service != DshService::Attachments
        );
    }
    let _offer = TestOffer::only(&[HostService::Remote]);
    assert!(super::dsh_broker::offered(DshService::Remote));
    assert!(!super::dsh_broker::offered(DshService::Network));
}

#[test]
fn a_record_that_cannot_be_read_at_the_scan_loads_no_owner() {
    let _policy = TestPolicyGuard::extension_host(true);
    let plugins = Plugins::new(&[("alpha", &[])]);
    let registry = plugins.registry();
    let (sources, errors) = codewhale_plugins::runtime::active_component_sources(
        &registry,
        codewhale_plugins::activation::PluginActivationCapability::Native,
    );
    assert_eq!(sources.len(), 1, "{errors:?}");
    plugins.registry().disable("alpha").unwrap();
    let (desired, errors) = super::desired_owners_from(&registry, sources, errors);
    assert!(desired.is_empty(), "{:?}", desired.keys());
    assert_eq!(
        errors
            .iter()
            .filter(|error| error
                .contains("`alpha` was not loaded because its review record could not be read"))
            .count(),
        1,
        "{errors:?}"
    );
}

#[tokio::test]
async fn no_broker_ticket_is_minted_for_a_plugin_whose_reviewed_record_does_not_name_the_service() {
    let _policy = TestPolicyGuard::extension_host(true);
    let offer = TestOffer::only(&[HostService::Network, HostService::Llm]);
    let granted = Plugins::new(&[("alpha", &["network"])]);
    let host_a = host(&granted.config.workspace);
    let alpha = host_a.load(&granted, "alpha");
    assert_eq!(host_a.services(&alpha), vec!["network"]);
    let guard = host_a.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&guard, "network");
    let (served, _) = host_a.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(served.unwrap()["service"], "network");
    let (wrong, _) = host_a.call(&echo(), &alpha, &network, "llm").await;
    assert_eq!(refusal(&wrong).1, "ticket_refused");

    let ungranted = Plugins::new(&[("beta", &[])]);
    let host_b = host(&ungranted.config.workspace);
    let beta = host_b.load(&ungranted, "beta");
    assert!(host_b.services(&beta).is_empty());
    let forged = OwnerRef {
        plugin_id: beta.plugin_id.clone(),
        ..alpha.clone()
    };
    let (borrowed, _) = host_b.call(&echo(), &forged, &network, "network").await;
    assert_eq!(refusal(&borrowed).1, "ticket_refused");

    let unshown = Plugins::new(&[("gamma", &["network"])]);
    unshown.review("gamma", false);
    let host_c = host(&unshown.config.workspace);
    let gamma = host_c.load(&unshown, "gamma");
    assert!(
        host_c.services(&gamma).is_empty(),
        "a review that did not show the service grants none"
    );

    drop(offer);
    let _none = TestOffer::only(&[]);
    assert!(
        host_a.services(&alpha).is_empty(),
        "a build that offers no service mints no broker ticket, whatever is recorded"
    );
    let (unreachable, _) = host_a.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(refusal(&unreachable), not_granted("network"));
    for service in DshService::ALL {
        let (stub, _) = host_a
            .call(super::dsh_broker::TABLE, &alpha, &network, service.name())
            .await;
        assert!(stub.is_err(), "{}", service.name());
    }
}

#[tokio::test]
async fn no_broker_ticket_while_a_plugin_without_the_grant_shares_the_host() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network, HostService::Llm]);
    let plugins = Plugins::new(&[
        ("alpha", &["network", "llm"]),
        ("beta", &[]),
        ("gamma", &["network"]),
    ]);
    plugins.review("alpha", true);
    let host = host(&plugins.config.workspace);
    let alpha = host.load(&plugins, "alpha");
    assert_eq!(host.services(&alpha), vec!["llm", "network"]);
    let alone = host.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&alone, "network");
    let llm = ticket(&alone, "llm");

    let gamma = host.load(&plugins, "gamma");
    assert_eq!(host.services(&alpha), vec!["network"]);
    assert_eq!(host.services(&gamma), vec!["network"]);
    let (shared, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(shared.unwrap()["plugin"], alpha.plugin_id);
    let (unshared, _) = host.call(&echo(), &alpha, &llm, "llm").await;
    assert_eq!(refusal(&unshared), not_granted("llm"));
    let report = host
        .manager
        .shared
        .registry
        .lock()
        .unwrap()
        .take_withheld_report()
        .unwrap();
    assert_eq!(
        report,
        "host service `llm` is off: the review of `gamma` does not include it"
    );
    assert!(
        host.manager
            .shared
            .registry
            .lock()
            .unwrap()
            .take_withheld_report()
            .is_none(),
        "reported once"
    );

    let beta = {
        let authority = plugins.registry().authority_for("beta").unwrap();
        host.manager
            .shared
            .registry
            .lock()
            .unwrap()
            .begin_owner(
                HostCohort::reviewed(),
                authority.plugin_id.as_str(),
                "beta",
                Some(authority.clone()),
                &authority.content_hash,
            )
            .unwrap()
    };
    assert!(
        host.services(&alpha).is_empty(),
        "an activating plugin without the grant already withholds it"
    );
    assert!(host.services(&gamma).is_empty());
    assert!(host.services(&beta).is_empty());
    let (withheld, _) = host.call(&echo(), &alpha, &network, "network").await;
    let own = Plugins::new(&[("delta", &[])]);
    let other_host = self::host(&own.config.workspace);
    let delta = other_host.load(&own, "delta");
    let lacking = other_host.mint(HostCohort::reviewed(), &delta);
    assert!(lacking.broker_tickets().is_none());
    assert_eq!(
        refusal(&withheld),
        not_granted("network"),
        "the refusal is the same whoever lacks the grant, and names nobody"
    );
    assert!(!refusal(&withheld).2.contains("beta"));
    let report = host
        .manager
        .shared
        .registry
        .lock()
        .unwrap()
        .take_withheld_report()
        .unwrap();
    assert_eq!(
        report,
        "host service `llm` is off: the reviews of `beta`, `gamma` do not include it; host service `network` is off: the review of `beta` does not include it"
    );
}

#[tokio::test]
async fn a_plugin_that_left_the_host_withholds_every_service_until_the_host_exits() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let plugins = Plugins::new(&[("alpha", &["network"]), ("gamma", &["network"])]);
    plugins.review("alpha", true);
    let host = host(&plugins.config.workspace);
    let alpha = host.load(&plugins, "alpha");
    let gamma = host.load(&plugins, "gamma");
    assert_eq!(host.services(&alpha), vec!["network"]);
    let guard = host.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&guard, "network");
    assert!(
        !host
            .manager
            .shared
            .registry
            .lock()
            .unwrap()
            .host_services_await_restart(HostCohort::reviewed())
    );

    {
        let mut registry = host.manager.shared.registry.lock().unwrap();
        assert_eq!(registry.revoke_owner(&gamma.plugin_id), Some(gamma.clone()));
        registry.forget_owner(&gamma.plugin_id);
        assert!(registry.host_services_await_restart(HostCohort::reviewed()));
        assert_eq!(
            registry.take_withheld_report().unwrap(),
            "host services are off until the plugin host restarts: a plugin that was turned off, changed or failed is still loaded in it"
        );
    }
    assert!(
        host.services(&alpha).is_empty(),
        "code that was loaded cannot be unloaded: nothing is given until the process is gone"
    );
    let (lingering, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(refusal(&lingering), not_granted("network"));

    let again = host.load(&plugins, "gamma");
    assert!(host.services(&alpha).is_empty());
    assert!(host.services(&again).is_empty());

    {
        let mut registry = host.manager.shared.registry.lock().unwrap();
        registry.host_exited(HostCohort::reviewed(), "planned restart");
        assert!(!registry.host_services_await_restart(HostCohort::reviewed()));
    }
    let alpha = host.load(&plugins, "alpha");
    assert_eq!(host.services(&alpha), vec!["network"]);

    let failing = host.load(&plugins, "gamma");
    host.manager
        .shared
        .registry
        .lock()
        .unwrap()
        .mark_failed(&failing, super::registry::OwnerState::Failed("boom".into()));
    assert!(host.services(&alpha).is_empty());
    host.manager
        .shared
        .registry
        .lock()
        .unwrap()
        .host_process_started(HostCohort::reviewed());
    assert_eq!(host.services(&alpha), vec!["network"]);
}

#[tokio::test]
async fn a_grant_is_read_from_the_owner_record_at_mint_and_at_redeem_never_from_the_wire() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network, HostService::Llm]);
    let plugins = Plugins::new(&[("alpha", &["network"])]);
    let host = host(&plugins.config.workspace);
    let alpha = host.load(&plugins, "alpha");
    let guard = host.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&guard, "network");
    let (served, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert!(served.is_ok(), "{served:?}");
    let (claimed, _) = host.call(&echo(), &alpha, &network, "llm").await;
    assert_eq!(
        refusal(&claimed).1,
        "ticket_refused",
        "input that claims a grant grants nothing"
    );

    plugins.review("alpha", false);
    let (reviewed_again, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(
        refusal(&reviewed_again),
        not_granted("network"),
        "a new review that drops the service refuses the next call of the running tool"
    );
    assert_eq!(
        host.services(&alpha),
        vec!["network"],
        "the owner record is replaced by reconcile; until then redeem reads the review record"
    );

    plugins.review("alpha", true);
    let (stale, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(
        refusal(&stale),
        not_granted("network"),
        "an owner from before the review holds nothing, whatever the new review grants"
    );
    host.manager
        .shared
        .registry
        .lock()
        .unwrap()
        .host_exited(HostCohort::reviewed(), "restart");
    let current = host.load(&plugins, "alpha");
    let guard = host.mint(HostCohort::reviewed(), &current);
    let (fresh, _) = host
        .call(&echo(), &current, &ticket(&guard, "network"), "network")
        .await;
    assert!(fresh.is_ok(), "{fresh:?}");

    std::fs::write(&plugins.config.state_path, b"{ not json").unwrap();
    let (unreadable, _) = host
        .call(&echo(), &current, &ticket(&guard, "network"), "network")
        .await;
    assert_eq!(refusal(&unreadable), not_granted("network"));
}

#[tokio::test]
async fn a_disable_from_another_process_stops_broker_calls_of_a_running_tool() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let plugins = Plugins::new(&[("alpha", &["network"]), ("gamma", &["network"])]);
    plugins.review("alpha", true);
    let host = host(&plugins.config.workspace);
    let alpha = host.load(&plugins, "alpha");
    let _gamma = host.load(&plugins, "gamma");
    let guard = host.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&guard, "network");
    let waiting: Vec<(DshService, Arm)> = vec![(DshService::Network, waiting_arm as Arm)];

    let in_flight = host.call(&waiting, &alpha, &network, "network");
    let elsewhere = async {
        until("the arm to start", || WAITING.load(Ordering::SeqCst)).await;
        plugins.registry().disable("gamma").unwrap();
    };
    let ((stopped, violations), ()) = tokio::join!(in_flight, elsewhere);
    assert_eq!(
        refusal(&stopped),
        not_granted("network"),
        "a co-hosted plugin turned off elsewhere withdraws the call in flight"
    );
    assert!(violations.is_empty());
    until("the arm's cancellation", || {
        WITHDRAWN.load(Ordering::SeqCst)
    })
    .await;

    let (next, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(refusal(&next), not_granted("network"));
    assert_eq!(
        host.services(&alpha),
        vec!["network"],
        "this process has not reconciled yet; the review record already refuses"
    );

    plugins.registry().enable("gamma").unwrap();
    let (spent, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(
        refusal(&spent),
        not_granted("network"),
        "the loaded owner is from before the plugin was turned off"
    );
    plugins.registry().disable("alpha").unwrap();
    let (own, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(refusal(&own), not_granted("network"));
}

#[tokio::test]
async fn a_builtin_tier_owner_never_gets_a_broker_ticket() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&HostService::ALL);
    let workspace = tempfile::tempdir().unwrap();
    let host = host(workspace.path());
    let builtin = {
        let mut registry = host.manager.shared.registry.lock().unwrap();
        let owner = registry
            .begin_owner(HostCohort::Builtin, "host:mcp", "mcp", None, "digest")
            .unwrap();
        registry.set_host_services(&owner, DshService::ALL.to_vec());
        registry.mark_active(&owner);
        assert!(
            DshService::ALL
                .into_iter()
                .all(|service| !registry.host_service_granted(&owner, service))
        );
        owner
    };
    assert!(host.manager.shared.broker_services(&builtin).is_empty());
    assert!(
        host.mint(HostCohort::Builtin, &builtin)
            .broker_tickets()
            .is_none()
    );
    let _everything = super::dsh_broker::TestGrants::only(&DshService::ALL);
    let forced = host
        .manager
        .shared
        .core_calls
        .begin(
            HostCohort::Builtin,
            HOST,
            &builtin,
            "call-2",
            host.gate.extension().unwrap().0,
            &host.context,
            &host.gate,
        )
        .unwrap();
    assert!(
        forced.broker_tickets().is_none(),
        "no override mints a broker ticket for the built-in tier"
    );
}

#[tokio::test]
async fn a_broker_ticket_is_refused_by_core_call_checkpoint_question_and_subagent() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let plugins = Plugins::new(&[("alpha", &["network"])]);
    let host = host(&plugins.config.workspace);
    let alpha = host.load(&plugins, "alpha");
    let guard = host.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&guard, "network");
    let shared = &host.manager.shared;
    let cx = |id| HostRequestContext::for_test(id).0;
    let refused = |result: Result<Value, RpcErrorWire>, what: &str| {
        let error = result.expect_err(what);
        assert_eq!(error.code, error_code::REFUSED, "{what}: {error:?}");
        assert_eq!(
            error.message, "the ticket is not valid for this request",
            "{what}"
        );
    };

    refused(
        shared
            .core_calls
            .serve(
                shared,
                HostCohort::reviewed(),
                HOST,
                super::protocol::CoreCallParams {
                    owner: alpha.clone(),
                    ticket: network.clone(),
                    name: "read_file".into(),
                    input: json!({ "path": "x" }),
                },
                cx(1),
            )
            .await,
        "core/call",
    );
    refused(
        shared
            .core_calls
            .serve_checkpoint(
                shared,
                HostCohort::reviewed(),
                HOST,
                super::protocol::SessionCheckpointParams {
                    owner: alpha.clone(),
                    ticket: network.clone(),
                },
                cx(2),
            )
            .await,
        "session/checkpoint",
    );
    refused(
        shared
            .core_calls
            .serve_question(
                shared,
                HostCohort::reviewed(),
                HOST,
                super::protocol::UserQuestionParams {
                    owner: alpha.clone(),
                    ticket: network.clone(),
                    questions: Vec::new(),
                },
                cx(3),
            )
            .await,
        "user-questions/ask",
    );
    refused(
        shared
            .core_calls
            .serve_subagent(
                shared,
                HostCohort::reviewed(),
                HOST,
                HostRequest::SubagentStart(super::protocol::SubagentStartParams {
                    owner: alpha.clone(),
                    ticket: network.clone(),
                    child_id: "child".into(),
                    prompt: "p".into(),
                    model: None,
                    persona: None,
                    denied_tools: Vec::new(),
                    delivery: "caller".into(),
                }),
                cx(4),
            )
            .await,
        "subagent/start",
    );
    let (still_good, _) = host.call(&echo(), &alpha, &network, "network").await;
    assert!(still_good.is_ok(), "{still_good:?}");
}

#[tokio::test]
async fn a_spent_or_just_ended_broker_ticket_is_a_limit_not_a_forgery() {
    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Network]);
    let plugins = Plugins::new(&[("alpha", &["network"])]);
    let host = host(&plugins.config.workspace);
    let alpha = host.load(&plugins, "alpha");
    let guard = host.mint(HostCohort::reviewed(), &alpha);
    let network = ticket(&guard, "network");
    for _ in 0..super::core_call::MAX_CALLS_PER_INVOCATION {
        let (served, _) = host.call(&echo(), &alpha, &network, "network").await;
        assert!(served.is_ok(), "{served:?}");
    }
    let (spent, violations) = host.call(&echo(), &alpha, &network, "network").await;
    assert_eq!(
        refusal(&spent),
        (
            error_code::REFUSED,
            "limit".to_string(),
            "this tool call's limit for the network service is used up".to_string()
        )
    );
    assert!(violations.is_empty());

    drop(guard);
    let mut ended = Vec::new();
    for _ in 0..(super::ticket::INVALID_BURST * 3) {
        let (late, violations) = host.call(&echo(), &alpha, &network, "network").await;
        assert_eq!(refusal(&late).1, "ticket_refused");
        ended.extend(violations);
    }
    assert!(
        ended.is_empty(),
        "frames already in the pipe when a tool call ends do not end the host: {ended:?}"
    );
    for _ in 0..super::ticket::INVALID_BURST {
        let (_, violations) = host.call(&echo(), &alpha, "cwt.guess", "network").await;
        ended.extend(violations);
    }
    assert_eq!(ended.len(), 1, "a guessed ticket still does: {ended:?}");
}
