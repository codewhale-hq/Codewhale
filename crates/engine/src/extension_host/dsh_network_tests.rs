use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use codewhale_config::OwnerRef;
use codewhale_net::network_policy::{Decision, NetworkPolicy, NetworkPolicyDecider};
use codewhale_secrets::InMemoryKeyringStore;
use tokio::sync::mpsc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::super::core_call::InvocationGuard;
use super::super::dsh_broker::{Arm, BrokerActTool, BrokerContext, REACH_TOOL, TestGrants};
use super::super::protocol::{DshBrokerParams, RpcErrorWire};
use super::super::supervisor::HostRequestContext;
use super::super::test_support::fake_authority;
use super::super::tier::HostTier;
use super::super::{ExtensionHostManager, ExtensionHostOptions};
use super::*;
use crate::tools::codemode::{
    ExtensionCaller, NestedCallGate, NestedCallRequest, NestedCallVerdict, NestedDecision,
};

const SECRET: &str = "sk-live-0123456789abcdef";
const BUILD: &str = "build-1";

thread_local! {
    static AUTHORITY: RefCell<Option<NetworkAuthority>> = const { RefCell::new(None) };
}

fn arm(call: BrokerCall) -> BrokerFuture {
    serve_with(
        call,
        AUTHORITY.with(|authority| authority.borrow().clone().expect("a test authority")),
    )
}

const TABLE: &[(DshService, Arm)] = &[(DshService::Network, arm as Arm)];

#[derive(Default)]
struct Person {
    cards: Mutex<Vec<(Value, String)>>,
    says_no: AtomicBool,
    absent: AtomicBool,
    act_denied: AtomicBool,
}

impl Person {
    fn cards(&self) -> Vec<(Value, String)> {
        self.cards.lock().unwrap().clone()
    }
}

fn serve_gate(mut requests: mpsc::Receiver<NestedCallRequest>, person: Arc<Person>) {
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            let NestedCallRequest::Tool(request) = request else {
                continue;
            };
            if request.is_stale() {
                continue;
            }
            let refuse = |decision| NestedCallVerdict::Refused {
                error: ToolError::permission_denied("the gate said no"),
                decision,
            };
            let verdict = if request.name == SEND_TOOL {
                let planned =
                    SendConsent.prepare(request.input.clone(), &ToolContext::new(Path::new(".")));
                match planned {
                    Err(_) => refuse(NestedDecision::Refused),
                    Ok(_) if person.absent.load(Ordering::SeqCst) => {
                        refuse(NestedDecision::Refused)
                    }
                    Ok(planned) => {
                        person
                            .cards
                            .lock()
                            .unwrap()
                            .push((request.input.clone(), planned.description));
                        if person.says_no.load(Ordering::SeqCst) {
                            refuse(NestedDecision::Denied)
                        } else {
                            NestedCallVerdict::Run {
                                name: request.name.clone(),
                                input: request.input.clone(),
                                supports_parallel: true,
                                decision: NestedDecision::Approved,
                                hook_context: None,
                            }
                        }
                    }
                }
            } else {
                let gate = if request.name == REACH_TOOL {
                    BrokerActTool::REACH
                } else {
                    BrokerActTool::ACT
                };
                match gate.prepare(request.input.clone(), &ToolContext::new(Path::new("."))) {
                    Err(_) => refuse(NestedDecision::Refused),
                    Ok(_) if person.act_denied.load(Ordering::SeqCst) => {
                        refuse(NestedDecision::Denied)
                    }
                    Ok(_) => NestedCallVerdict::Run {
                        name: request.name.clone(),
                        input: request.input.clone(),
                        supports_parallel: true,
                        decision: NestedDecision::Approved,
                        hook_context: None,
                    },
                }
            };
            let _ = request.reply.send(verdict);
        }
    });
}

struct Rig {
    manager: ExtensionHostManager,
    owner: OwnerRef,
    context: ToolContext,
    gate: NestedCallGate,
    person: Arc<Person>,
}

fn allowing(hosts: &[&str]) -> NetworkPolicyDecider {
    NetworkPolicyDecider::new(
        NetworkPolicy {
            default: Decision::Deny.into(),
            allow: hosts.iter().map(|host| (*host).to_string()).collect(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        },
        None,
    )
}

fn loopback() -> NetworkPolicyDecider {
    allowing(&["127.0.0.1"])
}

impl Rig {
    fn new(session: &str, shape: impl FnOnce(ToolContext) -> ToolContext) -> Self {
        let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
        let owner = {
            let mut registry = manager.shared.registry.lock().unwrap();
            let owner = registry
                .begin_owner(
                    HostTier::Plugin,
                    "a",
                    "a",
                    Some(fake_authority("a")),
                    "hash",
                )
                .unwrap();
            registry.mark_active(&owner);
            owner
        };
        let workspace = std::env::temp_dir();
        let context = ToolContext::new(&workspace)
            .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
                session.to_string(),
                "fixture-model".to_string(),
                workspace.clone(),
                None,
                Arc::new(Vec::new()),
            ))
            .with_origin_turn_id("turn");
        let (tx_event, mut rx_event) = mpsc::channel(8);
        tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
        let (gate, requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
        let person = Arc::new(Person::default());
        serve_gate(requests, Arc::clone(&person));
        let mut specs: Vec<Arc<dyn ToolSpec>> = BrokerActTool::all().to_vec();
        specs.push(Arc::new(SendConsent));
        Self {
            manager,
            owner,
            context: shape(context),
            person,
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

    fn begin(&self) -> InvocationGuard {
        let caller = self.gate.extension().unwrap().0.clone();
        self.manager
            .shared
            .core_calls
            .begin(
                HostTier::Plugin,
                7,
                &self.owner,
                "call-1",
                &caller,
                &self.context,
                &self.gate,
            )
            .expect("an invocation")
    }

    async fn call(
        &self,
        guard: &InvocationGuard,
        authority: &NetworkAuthority,
        method: &str,
        input: Value,
    ) -> Result<Value, RpcErrorWire> {
        let ticket = guard
            .broker_tickets()
            .expect("broker tickets")
            .into_iter()
            .find(|wire| wire.service == "network")
            .expect("a network ticket")
            .ticket;
        AUTHORITY.with(|slot| *slot.borrow_mut() = Some(authority.clone()));
        let (cx, _violations, _cancel) = HostRequestContext::for_test(1);
        self.manager
            .shared
            .core_calls
            .serve_broker_in(
                TABLE,
                &self.manager.shared,
                HostTier::Plugin,
                7,
                DshBrokerParams {
                    owner: self.owner.clone(),
                    ticket,
                    service: "network".to_string(),
                    method: method.to_string(),
                    input,
                },
                cx,
            )
            .await
    }

    async fn fetch(
        &self,
        guard: &InvocationGuard,
        authority: &NetworkAuthority,
        input: Value,
    ) -> Result<Value, RpcErrorWire> {
        self.call(guard, authority, "fetch", input).await
    }
}

fn vault() -> Vault {
    Vault {
        store: Secrets::new(Arc::new(InMemoryKeyringStore::new())),
        plaintext_for_test: true,
    }
}

fn authority(sites: &[String], vault: &Vault) -> NetworkAuthority {
    NetworkAuthority {
        build: BUILD.to_string(),
        sites: sites
            .iter()
            .map(|site| Origin::parse(site).expect("a site"))
            .collect(),
        vault: vault.clone(),
        test_dns_pins: BTreeMap::new(),
    }
}

fn site_for(host: &str, server: &MockServer) -> String {
    format!("http://{host}:{}", server.address().port())
}

fn authority_pinned(servers: &[(&str, &MockServer)], vault: &Vault) -> NetworkAuthority {
    let sites: Vec<String> = servers
        .iter()
        .map(|(host, server)| site_for(host, server))
        .collect();
    let mut out = authority(&sites, vault);
    out.test_dns_pins = servers
        .iter()
        .map(|(host, server)| ((*host).to_string(), server.address().ip()))
        .collect();
    out
}

fn keep(vault: &Vault, plugin: &str, name: &str, site: &str, value: &str) {
    assert!(
        vault
            .write_blocking(
                plugin,
                BUILD,
                name,
                Stored {
                    site: site.to_string(),
                    value: value.to_string(),
                },
            )
            .is_ok()
    );
}

fn kind(result: &Result<Value, RpcErrorWire>) -> (String, String) {
    let error = result.as_ref().expect_err("a refusal");
    (
        error
            .data
            .as_ref()
            .and_then(|data| data["dsh_refusal"].as_str())
            .unwrap_or("untyped")
            .to_string(),
        error.message.clone(),
    )
}

fn body(result: &Value) -> String {
    String::from_utf8(
        STANDARD
            .decode(result["body_base64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
}

fn detached(plugin: &str, method: &str, input: Value) -> BrokerCall {
    let owner = OwnerRef {
        plugin_id: plugin.into(),
        generation: 1,
        owner_token: "t".into(),
    };
    BrokerCall {
        context: BrokerContext::detached_for_test(&owner, DshService::Network),
        owner,
        session_id: "detached".into(),
        service: DshService::Network,
        method: method.into(),
        input,
    }
}

fn code(result: &Result<Value, BrokerRefusal>) -> &'static str {
    result.as_ref().expect_err("a refusal").code()
}

async fn ok_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    server
}

async fn quiet_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    server
}

struct Echo;
impl Respond for Echo {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let auth = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        ResponseTemplate::new(200)
            .insert_header("x-echo", auth.as_str())
            .set_body_string(format!("you sent {auth}"))
    }
}

type SeenRequests = Arc<Mutex<Vec<Vec<(String, String)>>>>;

#[derive(Clone, Default)]
struct Seen(SeenRequests);
impl Respond for Seen {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.0.lock().unwrap().push(
            request
                .headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect(),
        );
        ResponseTemplate::new(200).set_body_string("landed")
    }
}

#[test]
fn a_site_name_is_ascii_and_derived_from_the_url() {
    let site = |url: &str| Origin::parse(url).map(|site| site.to_string());
    assert_eq!(
        site("https://API.Example.com").as_deref(),
        Some("https://api.example.com")
    );
    assert_eq!(
        site("https://api.example.com:443/").as_deref(),
        Some("https://api.example.com")
    );
    assert_eq!(
        site("https://api.example.com:8443").as_deref(),
        Some("https://api.example.com:8443")
    );
    let named = site("https://b\u{fc}cher.example").expect("an IDN site");
    assert!(named.is_ascii() && named.contains("xn--"), "{named}");
    for refused in [
        "https://user:pw@api.example.com",
        "https://api.example.com/v1",
        "https://api.example.com?x=1",
        "https://api.example.com.",
        "ftp://api.example.com",
        "api.example.com",
    ] {
        assert_eq!(site(refused), None, "{refused}");
    }
    assert_ne!(
        Origin::parse("https://api.example.com"),
        Origin::parse("http://api.example.com")
    );
}

#[tokio::test]
async fn withheld_authority_refuses_typed_and_unknown_methods_are_named() {
    let unknown = serve(detached("a", "connect", json!({}))).await;
    assert_eq!(code(&unknown), "unknown_method");
    let bad = serve(detached("a", "fetch", json!({"url": 1}))).await;
    assert_eq!(code(&bad), "invalid_input");
    for (method, input) in [
        ("fetch", json!({"url": "https://example.com/"})),
        ("credentials.resolve", json!({"ref": "K"})),
        ("credentials.describe", json!({"ref": "K"})),
        (
            "credentials.set",
            json!({"ref": "K", "value": "v", "site": "https://example.com"}),
        ),
    ] {
        assert_eq!(
            code(&serve(detached("a", method, input)).await),
            "not_available",
            "{method}"
        );
    }
}

#[tokio::test]
async fn credentials_never_leave_rust_and_are_scoped_to_their_plugin() {
    let vault = vault();
    let site = "https://api.example.com".to_string();
    let authority = authority(std::slice::from_ref(&site), &vault);
    let set = serve_with(
        detached(
            "a",
            "credentials.set",
            json!({"ref": "OPENAI_API_KEY", "value": SECRET, "site": site}),
        ),
        authority.clone(),
    )
    .await
    .unwrap();
    assert!(!set.to_string().contains(SECRET));
    assert_eq!(set["site"], "https://api.example.com");
    for method in ["credentials.resolve", "credentials.describe"] {
        let answer = serve_with(
            detached("a", method, json!({"ref": "OPENAI_API_KEY"})),
            authority.clone(),
        )
        .await
        .unwrap();
        assert!(!answer.to_string().contains(SECRET), "{method}");
        assert!(
            answer
                .to_string()
                .contains("codewhale-credential:OPENAI_API_KEY"),
            "{method}"
        );
    }
    let foreign = serve_with(
        detached("b", "credentials.resolve", json!({"ref": "OPENAI_API_KEY"})),
        authority.clone(),
    )
    .await
    .unwrap();
    assert_eq!(foreign["value"], Value::Null);
    let traversal = serve_with(
        detached(
            "b",
            "credentials.describe",
            json!({"ref": "../a:OPENAI_API_KEY"}),
        ),
        authority.clone(),
    )
    .await;
    assert_eq!(code(&traversal), "invalid_input");
    let elsewhere = serve_with(
        detached(
            "a",
            "credentials.set",
            json!({"ref": "K", "value": SECRET, "site": "https://other.example.com"}),
        ),
        authority,
    )
    .await;
    assert_eq!(code(&elsewhere), "refused");
}

#[tokio::test]
async fn an_arm_whose_cancel_has_fired_has_no_effect() {
    let vault = vault();
    let site = "https://api.example.com".to_string();
    let authority = authority(std::slice::from_ref(&site), &vault);
    let call = detached(
        "a",
        "credentials.set",
        json!({"ref": "K", "value": SECRET, "site": site}),
    );
    call.context.cancel().cancel();
    assert_eq!(code(&serve_with(call, authority.clone()).await), "refused");
    assert!(vault.store.get(VAULT_ENTRY).unwrap().is_none());
    let call = detached("a", "fetch", json!({"url": "https://api.example.com/"}));
    call.context.cancel().cancel();
    assert_eq!(code(&serve_with(call, authority).await), "refused");
}

#[tokio::test]
async fn a_forged_consent_request_never_reaches_a_card() {
    let context = ToolContext::new(Path::new("."));
    let forged = json!({"request": "guess", "plugin": "a", "url": "https://bank.example/"});
    assert!(SendConsent.prepare(forged.clone(), &context).is_err());
    assert!(SendConsent.execute(forged, &context).await.is_err());
    assert!(
        SendConsent
            .prepare(json!({"url": "https://bank.example/"}), &context)
            .is_err()
    );
    assert!(!SendConsent.model_visible());
    assert!(
        SendConsent
            .capabilities()
            .contains(&ToolCapability::Network)
    );
    let request = PendingRequest::open();
    let live = json!({"request": request.0, "plugin": "a", "url": "https://bank.example/"});
    let planned = SendConsent.prepare(live.clone(), &context).unwrap();
    assert_eq!(planned.approval, ApprovalRequirement::Required);
    assert!(planned.description.contains("`a`"));
    assert!(planned.description.contains("https://bank.example"));
    drop(request);
    assert!(SendConsent.prepare(live, &context).is_err());
}

#[tokio::test]
async fn a_fetch_is_refused_when_the_session_has_no_network_policy() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let vault = vault();
    let authority = authority(&[server.uri()], &vault);
    let url = json!({"url": format!("{}/x", server.uri())});
    let prompt = NetworkPolicyDecider::new(
        NetworkPolicy {
            audit: false,
            ..NetworkPolicy::default()
        },
        None,
    );
    let denied = NetworkPolicyDecider::new(
        NetworkPolicy {
            default: Decision::Allow.into(),
            allow: Vec::new(),
            deny: vec!["127.0.0.1".to_string()],
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: false,
        },
        None,
    );
    for (name, policy) in [
        ("none", None),
        ("prompt", Some(prompt)),
        ("deny", Some(denied)),
    ] {
        let rig = Rig::new(&format!("no-policy-{name}"), |context| match policy {
            Some(policy) => context.with_network_policy(policy),
            None => context,
        });
        let guard = rig.begin();
        let result = rig.fetch(&guard, &authority, url.clone()).await;
        assert_eq!(
            kind(&result),
            ("refused".to_string(), NOT_ALLOWED_HERE.to_string()),
            "{name}"
        );
        assert!(rig.person.cards().is_empty(), "{name}: no card is raised");
    }
}

#[tokio::test]
async fn the_first_send_to_a_site_raises_one_card_and_later_sends_go_through() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = ok_server().await;
    let second = ok_server().await;
    let site = site_for("api.example.com", &server);
    let second_site = site_for("other.example.com", &second);
    let vault = vault();
    let authority = authority_pinned(
        &[("api.example.com", &server), ("other.example.com", &second)],
        &vault,
    );
    let rig = Rig::new("one-card", |context| {
        context.with_network_policy(allowing(&["api.example.com", "other.example.com"]))
    });
    let guard = rig.begin();
    for _ in 0..3 {
        let result = rig
            .fetch(&guard, &authority, json!({"url": format!("{site}/x")}))
            .await
            .unwrap();
        assert_eq!(result["status"], 200);
    }
    let cards = rig.person.cards();
    assert_eq!(cards.len(), 1, "{cards:?}");
    let (input, text) = &cards[0];
    assert_eq!(input["plugin"], "a");
    assert_eq!(input["url"], format!("{site}/"));
    assert!(text.contains("`a`") && text.contains(&site), "{text}");
    assert!(text.is_ascii());
    let receipts = guard.receipts().expect("the card is in the tool result");
    let send_calls = receipts["calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["tool"] == SEND_TOOL)
        .count();
    assert_eq!(send_calls, 1, "{receipts}");
    assert_eq!(receipts["broker_acts"]["total"], 3, "{receipts}");

    let url = json!({"url": format!("{second_site}/y")});
    let (one, two) = tokio::join!(
        rig.fetch(&guard, &authority, url.clone()),
        rig.fetch(&guard, &authority, url.clone())
    );
    assert_eq!(one.unwrap()["status"], 200);
    assert_eq!(two.unwrap()["status"], 200);
    assert_eq!(rig.person.cards().len(), 2, "one card for the second site");
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    assert_eq!(second.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_no_on_the_card_is_remembered_for_the_session() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = ok_server().await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let url = json!({"url": format!("{site}/x")});
    let rig = Rig::new("said-no", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    rig.person.says_no.store(true, Ordering::SeqCst);
    let first = rig.fetch(&guard, &authority, url.clone()).await;
    assert_eq!(kind(&first), ("denied".to_string(), DECLINED.to_string()));
    rig.person.says_no.store(false, Ordering::SeqCst);
    let again = rig.fetch(&guard, &authority, url.clone()).await;
    assert_eq!(kind(&again).0, "denied");
    assert_eq!(rig.person.cards().len(), 1, "a no is not asked twice");
    assert!(server.received_requests().await.unwrap().is_empty());

    let absent = Rig::new("nobody-there", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let absent_guard = absent.begin();
    absent.person.absent.store(true, Ordering::SeqCst);
    let unanswered = absent.fetch(&absent_guard, &authority, url.clone()).await;
    assert_eq!(
        kind(&unanswered),
        ("refused".to_string(), NOT_ALLOWED_HERE.to_string())
    );
    absent.person.absent.store(false, Ordering::SeqCst);
    assert_eq!(
        absent
            .fetch(&absent_guard, &authority, url.clone())
            .await
            .unwrap()["status"],
        200,
        "a card nobody answered is asked again"
    );

    let other = Rig::new("another-session", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let other_guard = other.begin();
    assert_eq!(
        other.fetch(&other_guard, &authority, url).await.unwrap()["status"],
        200
    );
    assert_eq!(other.person.cards().len(), 1, "another session asks again");
}

#[tokio::test]
async fn every_send_leaves_an_audit_receipt_without_body_or_credential() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(201).set_body_string("stored"))
        .mount(&server)
        .await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    keep(&vault, "a", "KEY", &site, SECRET);
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let session = format!("receipt-{}", uuid::Uuid::new_v4().simple());
    let rig = Rig::new(&session, |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let sent = "WORKSPACE-FILE-CONTENTS";
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({
                "url": format!("{site}/upload"),
                "method": "post",
                "headers": [["authorization", "Bearer codewhale-credential:KEY"]],
                "body_base64": STANDARD.encode(sent),
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], 201);
    let log = std::fs::read_to_string(
        codewhale_runtime::audit::audit_log_path().expect("an audit log path"),
    )
    .expect("the audit log");
    let lines: Vec<&str> = log.lines().filter(|line| line.contains(&session)).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let record: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(record["event"], AUDIT_EVENT);
    assert_eq!(
        record["details"],
        json!({
            "plugin": "a",
            "session": session,
            "site": site,
            "method": "POST",
            "bytes_out": sent.len(),
            "bytes_in": "stored".len(),
            "status": 201,
            "outcome": "ok",
        })
    );
    assert!(!lines[0].contains(SECRET) && !lines[0].contains(sent));
}

fn child_without_network(workspace: &Path) -> Arc<crate::tools::subagent::engine::ChildAuthority> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = codewhale_runtime::config::Config::default()
        .with_legacy_root(Some("test-key".to_string()), None);
    let client = crate::client::CodewhaleClient::new(&config).expect("a stub client");
    let manager = crate::tools::subagent::new_shared_subagent_manager(workspace.to_path_buf(), 2);
    let mut runtime = crate::tools::subagent::SubAgentRuntime::new(
        client,
        "deepseek-v4-flash".to_string(),
        ToolContext::new(workspace),
        true,
        None,
        manager,
    );
    runtime.worker_profile.permissions.network = false;
    crate::tools::subagent::engine::ChildAuthority::capture(
        runtime,
        Default::default(),
        "child".to_string(),
        "child".to_string(),
        None,
    )
}

#[tokio::test]
async fn a_fetch_is_refused_for_a_sub_agent_without_network() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let vault = vault();
    let authority = authority(&[server.uri()], &vault);
    let child = child_without_network(&std::env::temp_dir());
    assert!(child.network_is_denied());
    let rig = Rig::new("child-no-network", |mut context| {
        context.execution.child_host = Some(child);
        context.with_network_policy(loopback())
    });
    let guard = rig.begin();
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": format!("{}/x", server.uri())}),
        )
        .await;
    assert_eq!(
        kind(&result),
        ("refused".to_string(), NOT_ALLOWED_HERE.to_string())
    );
    assert!(rig.person.cards().is_empty());
}

#[tokio::test]
async fn a_fetch_is_refused_for_a_worker_whose_envelope_denies_network() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let vault = vault();
    let authority = authority(&[server.uri()], &vault);
    let rig = Rig::new("envelope-no-network", |mut context| {
        context.execution.tool_authority =
            Some(Arc::new(crate::tools::spec::ToolAuthorityEnvelope {
                schema_version: 1,
                owner: "worker".to_string(),
                authority: crate::tools::spec::ToolMutationAuthority::ReadOnly,
                network_access: Some(false),
                shell: crate::tools::spec::ToolShellAuthority::ReadOnly,
                verification: crate::tools::spec::ToolVerificationAuthority::None,
                writable_roots: Vec::new(),
                writable_files: Vec::new(),
                coordination_contracts: Vec::new(),
            }));
        context.with_network_policy(loopback())
    });
    let guard = rig.begin();
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": format!("{}/x", server.uri())}),
        )
        .await;
    assert_eq!(
        kind(&result),
        ("refused".to_string(), NOT_ALLOWED_HERE.to_string())
    );
    assert!(rig.person.cards().is_empty());
}

#[tokio::test]
async fn a_fetch_follows_the_sessions_posture() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let quiet = quiet_server().await;
    let vault = vault();
    let refused_authority = authority(&[quiet.uri()], &vault);
    type Shape = Box<dyn FnOnce(ToolContext) -> ToolContext>;
    fn shaped(shape: impl FnOnce(ToolContext) -> ToolContext + 'static) -> Shape {
        Box::new(shape)
    }
    let shapes: Vec<(&str, Shape)> = vec![
        (
            "never",
            shaped(|mut context| {
                context.execution.approval_mode = ApprovalMode::Never;
                context
            }),
        ),
        (
            "auto-review",
            shaped(|mut context| {
                context.execution.approval_mode = ApprovalMode::Auto;
                context
            }),
        ),
        (
            "plan",
            shaped(|context| context.with_shell_network_denied_hint("plan mode")),
        ),
        (
            "deny-list",
            shaped(|mut context| {
                context.execution.disallowed_tools = vec!["fetch_url".to_string()];
                context
            }),
        ),
        (
            "deny-list-of-the-send-itself",
            shaped(|mut context| {
                context.execution.disallowed_tools = vec![SEND_TOOL.to_string()];
                context
            }),
        ),
    ];
    for (name, shape) in shapes {
        let rig = Rig::new(&format!("posture-{name}"), |context| {
            shape(context.with_network_policy(loopback()))
        });
        let guard = rig.begin();
        let result = rig
            .fetch(
                &guard,
                &refused_authority,
                json!({"url": format!("{}/x", quiet.uri())}),
            )
            .await;
        assert_eq!(
            kind(&result),
            ("refused".to_string(), NOT_ALLOWED_HERE.to_string()),
            "{name}"
        );
        assert!(rig.person.cards().is_empty(), "{name}");
    }
    let open = ok_server().await;
    let open_site = site_for("api.example.com", &open);
    let allowed_authority = authority_pinned(&[("api.example.com", &open)], &vault);
    for (name, mode) in [
        ("ask", ApprovalMode::Suggest),
        ("accept-edits", ApprovalMode::AcceptEdits),
        ("full-access", ApprovalMode::Bypass),
    ] {
        let rig = Rig::new(&format!("posture-{name}"), |mut context| {
            context.execution.approval_mode = mode;
            context.with_network_policy(allowing(&["api.example.com"]))
        });
        let guard = rig.begin();
        let result = rig
            .fetch(
                &guard,
                &allowed_authority,
                json!({"url": format!("{open_site}/x")}),
            )
            .await
            .unwrap();
        assert_eq!(result["status"], 200, "{name}");
        assert_eq!(
            rig.person.cards().len(),
            1,
            "{name}: the card is still raised"
        );
    }
}

#[tokio::test]
async fn a_posture_narrowed_during_the_call_stops_later_fetches() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = ok_server().await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let workspace = std::env::temp_dir();
    let posture = crate::core::engine::LivePosture::for_tests(&workspace, ApprovalMode::Suggest);
    let live = posture.clone();
    let rig = Rig::new("narrowed", move |mut context| {
        context.execution.live_posture = Some(live);
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let url = json!({"url": format!("{site}/x")});
    assert_eq!(
        rig.fetch(&guard, &authority, url.clone()).await.unwrap()["status"],
        200
    );
    posture.switch_for_tests(ApprovalMode::Never);
    let later = rig.fetch(&guard, &authority, url.clone()).await;
    assert_eq!(
        kind(&later),
        ("refused".to_string(), NOT_ALLOWED_HERE.to_string())
    );
    posture.switch_for_tests(ApprovalMode::Suggest);
    assert_eq!(
        rig.fetch(&guard, &authority, url).await.unwrap()["status"],
        200
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn an_oversized_credential_is_refused_and_the_store_stays_bounded() {
    let vault = vault();
    let site = "https://api.example.com".to_string();
    let authority = authority(std::slice::from_ref(&site), &vault);
    for (name, value) in [
        ("too long", "x".repeat(MAX_SECRET_BYTES + 1)),
        ("too short", "x".repeat(MIN_SECRET_BYTES - 1)),
        ("a megabyte", "x".repeat(1024 * 1024)),
        ("a line break", format!("{SECRET}\r\nx-injected: 1")),
        ("a handle", format!("{CREDENTIAL_HANDLE_PREFIX}OTHER-KEY")),
    ] {
        let result = serve_with(
            detached(
                "a",
                "credentials.set",
                json!({"ref": "K", "value": value, "site": site}),
            ),
            authority.clone(),
        )
        .await;
        assert_eq!(code(&result), "invalid_input", "{name}");
    }
    assert!(vault.store.get(VAULT_ENTRY).unwrap().is_none());
    let longest = serve_with(
        detached(
            "a",
            "credentials.set",
            json!({"ref": "K", "value": "x".repeat(MAX_SECRET_BYTES), "site": site}),
        ),
        authority,
    )
    .await;
    assert!(longest.is_ok());
    let stored = vault.store.get(VAULT_ENTRY).unwrap().unwrap();
    assert!(stored.len() < MAX_SECRET_BYTES + 512, "{}", stored.len());
}

#[tokio::test]
async fn a_plugin_cannot_store_more_than_n_credentials() {
    let vault = vault();
    let site = "https://api.example.com".to_string();
    let authority = authority(std::slice::from_ref(&site), &vault);
    let set = |name: String| {
        serve_with(
            detached(
                "a",
                "credentials.set",
                json!({"ref": name, "value": SECRET, "site": site}),
            ),
            authority.clone(),
        )
    };
    for index in 0..MAX_REFS_PER_PLUGIN {
        assert!(set(format!("K{index}")).await.is_ok(), "{index}");
    }
    let one_more = set("ONE_MORE".to_string()).await;
    assert_eq!(code(&one_more), "refused");
    assert!(format!("{one_more:?}").contains(VAULT_FULL));
    assert!(
        set("K0".to_string()).await.is_ok(),
        "an existing ref is rewritten"
    );

    let total = self::vault();
    let value = "x".repeat(512);
    for index in 0..MAX_REFS_TOTAL {
        keep(
            &total,
            &format!("plugin-{}", index / MAX_REFS_PER_PLUGIN),
            &format!("K{}", index % MAX_REFS_PER_PLUGIN),
            &site,
            &value,
        );
    }
    let full = total.write_blocking(
        "one-plugin-too-many",
        BUILD,
        "K",
        Stored {
            site: site.clone(),
            value: value.clone(),
        },
    );
    assert!(matches!(full, Err(VaultError::Full)));
    let stored = total.store.get(VAULT_ENTRY).unwrap().unwrap();
    assert!(
        stored.len() < MAX_REFS_TOTAL * (512 + 256),
        "{}",
        stored.len()
    );
}

#[tokio::test]
async fn headers_are_capped_after_substitution() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    keep(&vault, "a", "BIG", &site, &"x".repeat(MAX_SECRET_BYTES));
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("header-cap", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let url = format!("{site}/x");
    let handles = |count: usize| -> Vec<Value> {
        (0..count)
            .map(|index| json!([format!("x-key-{index}"), "codewhale-credential:BIG"]))
            .collect()
    };
    let filled = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": url, "headers": handles(5)}),
        )
        .await;
    let (code, message) = kind(&filled);
    assert_eq!(code, "invalid_input");
    assert!(
        message.contains("once credentials are filled in"),
        "{message}"
    );
    let many = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": url, "headers": handles(MAX_HANDLES_PER_REQUEST + 1)}),
        )
        .await;
    let (code, message) = kind(&many);
    assert_eq!(code, "invalid_input");
    assert!(message.contains("credential handles"), "{message}");
}

#[test]
fn scrubbing_is_linear_and_bounded() {
    let names: Vec<String> = (0..MAX_HANDLES_PER_REQUEST)
        .map(|index| handle(&format!("K{index}")))
        .collect();
    let near: Vec<(String, String)> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut secret = "a".repeat(MAX_SECRET_BYTES - 1);
            secret.push(char::from(b'b' + index as u8));
            (name.clone(), secret)
        })
        .collect();
    let haystack = vec![b'a'; MAX_RESPONSE_BYTES];
    let started = Instant::now();
    let clean = scrub(&haystack, &near, MAX_RESPONSE_BYTES).expect("within the cap");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(clean.len(), haystack.len());

    let short = vec![("codewhale-credential:S".to_string(), "aaaaaaaa".to_string())];
    let run = scrub(&vec![b'a'; 1024 * 1024], &short, 64).expect("one run is one handle");
    assert_eq!(run, b"codewhale-credential:S");
    let spaced = "aaaaaaaa-".repeat(1000);
    assert!(scrub(spaced.as_bytes(), &short, spaced.len()).is_none());
    let within = scrub(spaced.as_bytes(), &short, 1024 * 1024).unwrap();
    assert_eq!(
        String::from_utf8(within).unwrap(),
        "codewhale-credential:S-".repeat(1000)
    );

    let nested = vec![
        (
            "codewhale-credential:INNER".to_string(),
            "secretBBBB".to_string(),
        ),
        (
            "codewhale-credential:OUTER".to_string(),
            "XXXXXXXXsecretBBBBYYYY".to_string(),
        ),
    ];
    let both = scrub(b"a XXXXXXXXsecretBBBBYYYY b secretBBBB c", &nested, 1024).unwrap();
    assert_eq!(
        String::from_utf8(both).unwrap(),
        "a codewhale-credential:OUTER b codewhale-credential:INNER c"
    );
    let overlapping = vec![("codewhale-credential:O".to_string(), "abababab".to_string())];
    let folded = scrub(b"<ababababab>", &overlapping, 1024).unwrap();
    assert_eq!(folded, b"<codewhale-credential:O>");
    assert_eq!(scrub(b"plain", &[], 1024).unwrap(), b"plain");
}

#[tokio::test]
async fn the_runtime_stays_responsive_while_a_fetch_scrubs() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {SECRET}").as_str()))
        .respond_with(Echo)
        .expect(1)
        .mount(&server)
        .await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    keep(&vault, "a", "KEY", &site, SECRET);
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("scrub-thread", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    *SCRUBBED_ON.lock().unwrap() = None;
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({
                "url": format!("{site}/v1/models"),
                "headers": [["authorization", "Bearer codewhale-credential:KEY"]],
            }),
        )
        .await
        .unwrap();
    assert!(!result.to_string().contains(SECRET));
    assert_eq!(body(&result), "you sent Bearer codewhale-credential:KEY");
    assert!(
        result["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pair| pair[0] == "x-echo" && pair[1] == "Bearer codewhale-credential:KEY")
    );
    let scrubbed_on = SCRUBBED_ON.lock().unwrap().expect("the scrub ran");
    assert_ne!(
        scrubbed_on,
        std::thread::current().id(),
        "the scrub runs off the thread that drives this single-threaded runtime"
    );
}

#[tokio::test]
async fn at_most_n_fetches_run_at_once_per_invocation() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = MockServer::start().await;
    let delay = Duration::from_millis(400);
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(delay))
        .mount(&server)
        .await;
    Mock::given(path("/first"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("four-at-once", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    rig.fetch(&guard, &authority, json!({"url": format!("{site}/first")}))
        .await
        .unwrap();
    let started = Instant::now();
    let results = futures_util::future::join_all(
        (0..MAX_FETCHES_AT_ONCE * 2)
            .map(|_| rig.fetch(&guard, &authority, json!({"url": format!("{site}/slow")}))),
    )
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert!(
        started.elapsed() >= delay * 2 - Duration::from_millis(50),
        "eight fetches, four at once, take two rounds: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_credential_is_sent_only_to_the_origin_it_was_bound_to() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let home = MockServer::start().await;
    Mock::given(header("authorization", SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&home)
        .await;
    let other = quiet_server().await;
    let home_site = site_for("api.example.com", &home);
    let other_site = site_for("other.example.com", &other);
    let vault = vault();
    let authority = authority_pinned(
        &[("api.example.com", &home), ("other.example.com", &other)],
        &vault,
    );
    let rig = Rig::new("bound-origin", |context| {
        context.with_network_policy(allowing(&["api.example.com", "other.example.com"]))
    });
    let guard = rig.begin();
    rig.call(
        &guard,
        &authority,
        "credentials.set",
        json!({"ref": "KEY", "value": SECRET, "site": home_site}),
    )
    .await
    .unwrap();
    let headers = json!([["authorization", "codewhale-credential:KEY"]]);
    let at_home = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": format!("{home_site}/x"), "headers": headers}),
        )
        .await
        .unwrap();
    assert_eq!(at_home["status"], 200);
    let elsewhere = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": format!("{other_site}/x"), "headers": headers}),
        )
        .await;
    assert_eq!(
        kind(&elsewhere),
        ("refused".to_string(), NOT_FOR_THIS_SITE.to_string())
    );
    assert_eq!(
        rig.person.cards().len(),
        1,
        "no card for a send that is refused"
    );
}

#[tokio::test]
async fn a_credential_is_never_sent_over_plain_http() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let site = site_for("api.example.com", &server);
    let strict = Vault {
        plaintext_for_test: false,
        ..vault()
    };
    let authority = authority_pinned(&[("api.example.com", &server)], &strict);
    let rig = Rig::new("https-only", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let stored = rig
        .call(
            &guard,
            &authority,
            "credentials.set",
            json!({"ref": "KEY", "value": SECRET, "site": site}),
        )
        .await;
    assert_eq!(kind(&stored).0, "invalid_input");
    keep(&strict, "a", "KEY", &site, SECRET);
    let sent = rig
        .fetch(
            &guard,
            &authority,
            json!({
                "url": format!("{site}/x"),
                "headers": [["authorization", "codewhale-credential:KEY"]],
            }),
        )
        .await;
    assert_eq!(
        kind(&sent),
        ("refused".to_string(), NOT_FOR_THIS_SITE.to_string())
    );
}

#[tokio::test]
async fn another_port_of_a_reviewed_host_is_refused() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let reviewed = quiet_server().await;
    let other_port = quiet_server().await;
    let reviewed_site = site_for("api.example.com", &reviewed);
    let other_site = site_for("api.example.com", &other_port);
    let vault = vault();
    let authority = authority_pinned(&[("api.example.com", &reviewed)], &vault);
    let rig = Rig::new("other-port", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    for url in [
        format!("{other_site}/x"),
        reviewed_site.replace("http://", "https://"),
        reviewed_site.replace("api.example.com", "localhost"),
        "https://unreviewed.example.com/".to_string(),
    ] {
        let result = rig.fetch(&guard, &authority, json!({"url": url})).await;
        assert_eq!(
            kind(&result),
            ("refused".to_string(), NOT_REVIEWED.to_string()),
            "{url}"
        );
    }
    assert!(rig.person.cards().is_empty());
}

#[tokio::test]
async fn a_compressed_or_partial_echo_never_returns_the_secret() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = ok_server().await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    keep(&vault, "a", "KEY", &site, SECRET);
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("echo-shapes", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let url = format!("{site}/x");
    for (name, value) in [
        ("Accept-Encoding", "gzip"),
        ("Range", "bytes=0-10"),
        ("If-Range", "\"etag\""),
    ] {
        let result = rig
            .fetch(
                &guard,
                &authority,
                json!({
                    "url": url,
                    "headers": [["authorization", "codewhale-credential:KEY"], [name, value]],
                }),
            )
            .await;
        let (code, message) = kind(&result);
        assert_eq!(code, "invalid_input", "{name}");
        assert!(message.contains("together with a credential"), "{message}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    let plain = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": url, "headers": [["Accept-Encoding", "gzip"], ["Range", "bytes=0-1"]]}),
        )
        .await
        .unwrap();
    assert_eq!(plain["status"], 200, "without a credential they are sent");
}

#[tokio::test]
async fn the_final_url_is_scrubbed() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = MockServer::start().await;
    Mock::given(path("/start"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("/landing?token={SECRET}").as_str()),
        )
        .mount(&server)
        .await;
    Mock::given(path("/landing"))
        .and(header("authorization", SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    keep(&vault, "a", "KEY", &site, SECRET);
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("final-url", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({
                "url": format!("{site}/start"),
                "headers": [["authorization", "codewhale-credential:KEY"]],
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["redirected"], true);
    assert!(!result.to_string().contains(SECRET), "{result}");
    assert_eq!(
        result["url"],
        format!("{site}/landing?token=codewhale-credential:KEY")
    );
    assert_eq!(
        rig.person.cards().len(),
        1,
        "a redirect on the same site asks once"
    );
}

#[tokio::test]
async fn a_reinstalled_plugin_does_not_inherit_credentials() {
    let vault = vault();
    let site = "https://api.example.com".to_string();
    keep(&vault, "a", "OLD", &site, SECRET);
    let reinstalled = NetworkAuthority {
        build: "build-2".to_string(),
        ..authority(std::slice::from_ref(&site), &vault)
    };
    let resolved = serve_with(
        detached("a", "credentials.resolve", json!({"ref": "OLD"})),
        reinstalled.clone(),
    )
    .await
    .unwrap();
    assert_eq!(resolved["value"], Value::Null);
    let described = serve_with(
        detached("a", "credentials.describe", json!({"ref": "OLD"})),
        reinstalled.clone(),
    )
    .await
    .unwrap();
    assert_eq!(described["set"], false);
    assert!(
        vault
            .read_blocking("a", "build-2")
            .is_ok_and(|slots| slots.is_empty())
    );
    serve_with(
        detached(
            "a",
            "credentials.set",
            json!({"ref": "NEW", "value": "another-secret-value", "site": site}),
        ),
        reinstalled,
    )
    .await
    .unwrap();
    assert!(
        !vault
            .store
            .get(VAULT_ENTRY)
            .unwrap()
            .unwrap()
            .contains(SECRET),
        "the first build's slots are gone once the new build writes"
    );
    assert!(
        vault
            .read_blocking("a", BUILD)
            .is_ok_and(|slots| slots.is_empty())
    );
}

#[test]
fn uninstall_deletes_a_plugins_credentials() {
    let vault = vault();
    keep(&vault, "a", "KEY", "https://api.example.com", SECRET);
    keep(
        &vault,
        "b",
        "KEY",
        "https://api.example.com",
        "the-other-plugins-key",
    );
    assert!(vault.forget_blocking("a").is_ok());
    assert!(
        vault
            .read_blocking("a", BUILD)
            .is_ok_and(|slots| slots.is_empty())
    );
    assert!(
        vault
            .read_blocking("b", BUILD)
            .is_ok_and(|slots| slots.contains_key("KEY"))
    );
    assert!(
        !vault
            .store
            .get(VAULT_ENTRY)
            .unwrap()
            .unwrap()
            .contains(SECRET)
    );
    assert!(vault.forget_blocking("never-installed").is_ok());
}

#[test]
fn the_vault_is_its_own_private_file() {
    let home = tempfile::tempdir().unwrap();
    let file = vault_path(home.path());
    assert_eq!(
        file,
        home.path().join("secrets").join("plugin-credentials.json")
    );
    let vault = Vault::at(file.clone());
    keep(&vault, "a", "KEY", "https://api.example.com", SECRET);
    assert!(file.is_file());
    assert!(
        !home.path().join("secrets").join("secrets.json").exists(),
        "the provider key file is not touched"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
    let reopened = Vault::at(file);
    assert!(
        reopened
            .read_blocking("a", BUILD)
            .is_ok_and(|slots| slots["KEY"].value == SECRET)
    );
}

#[tokio::test]
async fn an_unreadable_vault_refuses_the_send() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    vault.store.set(VAULT_ENTRY, "{ not json").unwrap();
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("bad-vault", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    let guard = rig.begin();
    let sent = rig
        .fetch(
            &guard,
            &authority,
            json!({
                "url": format!("{site}/x"),
                "headers": [["authorization", "codewhale-credential:KEY"]],
            }),
        )
        .await;
    assert_eq!(
        kind(&sent),
        ("refused".to_string(), VAULT_UNREADABLE.to_string())
    );
    let stored = rig
        .call(
            &guard,
            &authority,
            "credentials.set",
            json!({"ref": "KEY", "value": SECRET, "site": site}),
        )
        .await;
    assert_eq!(kind(&stored).0, "refused");
    assert_eq!(
        vault.store.get(VAULT_ENTRY).unwrap().as_deref(),
        Some("{ not json"),
        "a store that cannot be read is not overwritten"
    );
}

#[tokio::test]
async fn a_redirect_to_another_site_is_a_new_site_with_its_own_card() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let landing = Seen::default();
    let second = MockServer::start().await;
    Mock::given(path("/landing"))
        .respond_with(landing.clone())
        .mount(&second)
        .await;
    let first = MockServer::start().await;
    let first_site = site_for("api.example.com", &first);
    let second_site = site_for("other.example.com", &second);
    Mock::given(path("/away"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{second_site}/landing").as_str()),
        )
        .mount(&first)
        .await;
    let vault = vault();
    keep(&vault, "a", "KEY", &first_site, SECRET);
    let authority = authority_pinned(
        &[("api.example.com", &first), ("other.example.com", &second)],
        &vault,
    );
    let rig = Rig::new("redirect-new-site", |context| {
        context.with_network_policy(allowing(&["api.example.com", "other.example.com"]))
    });
    let guard = rig.begin();
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({
                "url": format!("{first_site}/away"),
                "headers": [
                    ["authorization", "Bearer codewhale-credential:KEY"],
                    ["x-key", "codewhale-credential:KEY"],
                    ["cookie", "typed-by-the-plugin"],
                    ["x-plain", "1"],
                ],
            }),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], 200);
    assert_eq!(result["redirected"], true);
    assert_eq!(body(&result), "landed");
    let cards = rig.person.cards();
    assert_eq!(cards.len(), 2, "{cards:?}");
    assert_eq!(cards[0].0["url"], format!("{first_site}/"));
    assert_eq!(cards[1].0["url"], format!("{second_site}/"));
    let seen = landing.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let names: Vec<&str> = seen[0].iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&"x-plain"), "{names:?}");
    for dropped in ["authorization", "x-key", "cookie"] {
        assert!(!names.contains(&dropped), "{dropped} followed the redirect");
    }
    assert!(seen[0].iter().all(|(_, value)| !value.contains(SECRET)));
}

#[tokio::test]
async fn a_redirect_never_reaches_a_site_the_person_declined() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let second = quiet_server().await;
    let first = MockServer::start().await;
    let first_site = site_for("api.example.com", &first);
    let second_site = site_for("other.example.com", &second);
    Mock::given(path("/away"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{second_site}/landing").as_str()),
        )
        .mount(&first)
        .await;
    let vault = vault();
    let authority = authority_pinned(
        &[("api.example.com", &first), ("other.example.com", &second)],
        &vault,
    );
    let rig = Rig::new("redirect-declined", |context| {
        context.with_network_policy(allowing(&["api.example.com", "other.example.com"]))
    });
    let guard = rig.begin();
    let landing = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": format!("{first_site}/landing")}),
        )
        .await;
    assert!(landing.is_ok(), "the first site is approved");
    rig.person.says_no.store(true, Ordering::SeqCst);
    let result = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": format!("{first_site}/away")}),
        )
        .await;
    assert_eq!(kind(&result), ("denied".to_string(), DECLINED.to_string()));
}

#[tokio::test]
async fn a_redirect_to_an_unreviewed_site_is_refused() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let unreviewed = quiet_server().await;
    let first = MockServer::start().await;
    let first_site = site_for("api.example.com", &first);
    Mock::given(path("/away"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/steal", unreviewed.uri()).as_str()),
        )
        .mount(&first)
        .await;
    Mock::given(path("/rename"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            first_site.replace("api.example.com", "localhost").as_str(),
        ))
        .mount(&first)
        .await;
    let vault = vault();
    let authority = authority_pinned(&[("api.example.com", &first)], &vault);
    let rig = Rig::new("redirect-unreviewed", |context| {
        context.with_network_policy(allowing(&["api.example.com", "127.0.0.1", "localhost"]))
    });
    let guard = rig.begin();
    for route in ["away", "rename"] {
        let result = rig
            .fetch(
                &guard,
                &authority,
                json!({"url": format!("{first_site}/{route}")}),
            )
            .await;
        assert_eq!(
            kind(&result),
            ("refused".to_string(), NOT_REVIEWED.to_string()),
            "{route}"
        );
    }
    assert_eq!(rig.person.cards().len(), 1);
}

#[tokio::test]
async fn an_egress_refusal_reports_no_resolved_address() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let local = quiet_server().await;
    let localhost_site = local.uri().replace("127.0.0.1", "localhost");
    let vault = vault();
    let metadata = "http://169.254.169.254".to_string();
    let authority = authority(
        &[metadata.clone(), local.uri(), localhost_site.clone()],
        &vault,
    );
    let rig = Rig::new("no-address", |context| {
        context.with_network_policy(allowing(&["169.254.169.254", "127.0.0.1", "localhost"]))
    });
    let guard = rig.begin();
    for target in [metadata, local.uri(), localhost_site] {
        let result = rig
            .fetch(
                &guard,
                &authority,
                json!({"url": format!("{target}/latest")}),
            )
            .await;
        let (code, message) = kind(&result);
        assert_eq!(code, "refused", "{target}");
        assert_eq!(message, NOT_ALLOWED_HERE, "{target}");
        for leak in [
            "169.254",
            "127.0.0.1",
            "localhost",
            "restricted",
            "resolved",
            "/network",
            "policy",
        ] {
            assert!(!message.contains(leak), "{message}");
        }
    }
    assert!(rig.person.cards().is_empty());
}

#[tokio::test]
async fn a_host_with_a_trailing_dot_is_refused_before_any_lookup() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let vault = vault();
    let authority = authority(&["http://example.com".to_string()], &vault);
    let rig = Rig::new("trailing-dot", |context| {
        context.with_network_policy(allowing(&["example.com"]))
    });
    let guard = rig.begin();
    for url in [
        "http://example.com./",
        "http://user:pw@example.com/",
        "ftp://example.com/",
    ] {
        let result = rig.fetch(&guard, &authority, json!({"url": url})).await;
        assert_eq!(kind(&result).0, "invalid_input", "{url}");
    }
    let in_url = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": "http://example.com/?k=codewhale-credential:KEY"}),
        )
        .await;
    assert_eq!(kind(&in_url).0, "invalid_input");
    let host_header = rig
        .fetch(
            &guard,
            &authority,
            json!({"url": "http://example.com/", "headers": [["Host", "evil.example"]]}),
        )
        .await;
    assert_eq!(kind(&host_header).0, "invalid_input");
    assert!(rig.person.cards().is_empty());
}

#[tokio::test]
async fn the_client_connects_only_to_the_pinned_address() {
    let server = MockServer::start().await;
    Mock::given(path("/pin"))
        .and(header(
            "host",
            format!("pinned.invalid:{}", server.address().port()).as_str(),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = pinned_client(
        Some(("pinned.invalid".to_string(), server.address().ip())),
        Duration::from_secs(10),
    )
    .unwrap();
    let response = client
        .get(format!(
            "http://pinned.invalid:{}/pin",
            server.address().port()
        ))
        .send()
        .await
        .expect("the pinned address answers for a name no resolver knows");
    assert_eq!(response.status().as_u16(), 204);
}

#[test]
fn the_first_send_card_cannot_be_skipped_by_a_grant_or_a_posture() {
    use super::super::core_call::{OriginApproval, origin_approval, refusal};
    use crate::core::authority::{
        ApprovalRequestDisposition as D, TurnAuthority, resolve_approval_request_disposition,
    };
    use codewhale_config::AppMode;

    let input = json!({"request": "r", "plugin": "a", "url": "https://api.example.com/"});
    let spec: Arc<dyn ToolSpec> = Arc::new(SendConsent);
    assert_eq!(
        origin_approval(SEND_TOOL, &input, true, Some(spec.as_ref())),
        OriginApproval::ForcePrompt
    );
    assert_eq!(
        refusal(std::slice::from_ref(&spec), SEND_TOOL, &input),
        None
    );
    let posture = |auto_approve: bool, mode: ApprovalMode, granted: bool| {
        resolve_approval_request_disposition(
            &TurnAuthority::from_effective_fields(AppMode::Agent, true, false, auto_approve, mode),
            granted,
            false,
            true,
            true,
        )
    };
    for granted in [false, true] {
        assert_eq!(posture(false, ApprovalMode::Suggest, granted), D::Prompt);
        assert_eq!(posture(true, ApprovalMode::Bypass, granted), D::Prompt);
        assert_eq!(
            posture(false, ApprovalMode::Auto, granted),
            D::AutoDenyAutoReview
        );
        assert_eq!(
            posture(false, ApprovalMode::Never, granted),
            D::AutoDenyNeverPosture
        );
    }
}

#[tokio::test]
async fn a_denied_service_act_leaves_credentials_and_network_uncalled_and_records_the_act() {
    let _grants = TestGrants::only(&[DshService::Network]);
    let server = quiet_server().await;
    let site = site_for("api.example.com", &server);
    let vault = vault();
    let authority = authority_pinned(&[("api.example.com", &server)], &vault);
    let rig = Rig::new("denied-act", |context| {
        context.with_network_policy(allowing(&["api.example.com"]))
    });
    rig.person.act_denied.store(true, Ordering::SeqCst);
    let guard = rig.begin();

    let write = rig
        .call(
            &guard,
            &authority,
            "credentials.set",
            json!({"ref": "KEY", "value": SECRET, "site": site}),
        )
        .await;
    assert_eq!(kind(&write).0, "denied");
    assert!(vault.store.get(VAULT_ENTRY).unwrap().is_none());

    let reach = rig
        .fetch(&guard, &authority, json!({"url": format!("{site}/x")}))
        .await;
    assert_eq!(kind(&reach).0, "denied");
    assert!(rig.person.cards().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());

    let described = rig
        .call(
            &guard,
            &authority,
            "credentials.describe",
            json!({"ref": "KEY"}),
        )
        .await
        .unwrap();
    assert_eq!(described["set"], false);

    let receipts = guard.receipts().expect("acts recorded");
    let acts = &receipts["broker_acts"];
    assert_eq!(acts["total"], 3, "{receipts}");
    assert_eq!(
        acts["acts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (
                entry["service"].as_str().unwrap(),
                entry["method"].as_str().unwrap(),
                entry["effect"].as_str().unwrap(),
                entry["outcome"].as_str().unwrap(),
                entry["target"].as_str().unwrap(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("network", "credentials.set", "write", "denied", "KEY"),
            ("network", "fetch", "reach", "denied", "api.example.com"),
            ("network", "credentials.describe", "read", "read", "KEY"),
        ]
    );
}
